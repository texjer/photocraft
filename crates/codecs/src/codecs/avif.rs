//! AVIF decoding: File › Open's still images and animated sequences, and Save for Web's preview of
//! the files this crate writes.
//!
//! Decoding uses rav1d (the Rust port of dav1d, pinned to a revision of the official repository
//! and built without its assembly, so it is pure Rust), avif-parse for a still image's items and
//! [`super::avif_boxes`] for the properties and the image sequence avif-parse doesn't read, with
//! the `avif` feature. The primary item's `colr` (ICC or CICP), `irot` and `imir` are applied;
//! grid images and Exif/XMP items are not read yet. Without the feature every entry point is a
//! [`CodecError::Unsupported`] error.

use crate::Format;
use crate::error::CodecError;
use crate::image::Image;
#[cfg(feature = "avif")]
use crate::options::DecodeOptions;

const F: Format = Format::Avif;

#[cfg(not(feature = "avif"))]
fn missing() -> CodecError {
    CodecError::unsupported(F, "AVIF isn't included in this build of PhotoCraft")
}

#[cfg(not(feature = "avif"))]
pub(crate) fn decode_preview(_bytes: &[u8]) -> Result<Image, CodecError> {
    Err(missing())
}

#[cfg(not(feature = "avif"))]
pub(crate) fn decode(_bytes: &[u8], _opts: &crate::options::DecodeOptions) -> Result<Image, CodecError> {
    Err(missing())
}

#[cfg(not(feature = "avif"))]
pub(crate) fn is_sequence(_bytes: &[u8]) -> bool {
    false
}

#[cfg(not(feature = "avif"))]
pub(crate) fn decode_frames(_bytes: &[u8], _opts: &crate::options::DecodeOptions) -> Result<Vec<super::gif_frames::AnimationFrame>, CodecError> {
    Err(missing())
}

/// One decoded AV1 frame: full-resolution luma, chroma at the layout's subsampling.
#[cfg(feature = "avif")]
struct Frame {
    width: u32,
    height: u32,
    /// Chroma subsampling shifts (x, y); `None` for monochrome.
    sub: Option<(u32, u32)>,
    chroma_width: u32,
    bits: u32,
    full_range: bool,
    matrix: rav1d::pixel::MatrixCoefficients,
    y: Vec<u16>,
    u: Vec<u16>,
    v: Vec<u16>,
}

/// An AV1 decoder for one stream (a still item, or every frame of a track in order).
#[cfg(feature = "avif")]
struct Av1 {
    decoder: rav1d::Decoder,
}

#[cfg(feature = "avif")]
fn rav1d_err(e: rav1d::Rav1dError) -> CodecError {
    CodecError::malformed(F, format!("AV1: {e:?}"))
}

/// Runs `f`, turning a panic inside the decoder (a file it can't cope with) into a malformed-file
/// error, as the HEIF decoder's calls are guarded.
#[cfg(feature = "avif")]
fn guarded<T>(f: impl FnOnce() -> Result<T, CodecError>) -> Result<T, CodecError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(CodecError::malformed(F, "the AV1 decoder failed on this file")))
}

#[cfg(feature = "avif")]
impl Av1 {
    fn new(limits: &crate::options::Limits) -> Result<Self, CodecError> {
        let mut settings = rav1d::Settings::new();
        // One frame in, one frame out: a frame is handed back as soon as it is decoded.
        settings.set_max_frame_delay(1);
        settings.set_n_threads(1);
        settings.set_frame_size_limit(u32::try_from(limits.max_pixels).unwrap_or(u32::MAX));
        Ok(Self { decoder: rav1d::Decoder::with_settings(&settings).map_err(rav1d_err)? })
    }

    /// Decodes the next temporal unit, one shown frame.
    fn decode(&mut self, obu: &[u8]) -> Result<Frame, CodecError> {
        use rav1d::Rav1dError::TryAgain;
        let mut picture = None;
        let mut sent = self.decoder.send_data(obu.to_vec().into_boxed_slice(), None, None, None);
        while matches!(sent, Err(TryAgain)) {
            // The decoder wants its finished picture taken before it reads on.
            match self.decoder.get_picture() {
                Ok(p) => picture = picture.or(Some(p)),
                Err(TryAgain) => {}
                Err(e) => return Err(rav1d_err(e)),
            }
            sent = self.decoder.send_pending_data();
        }
        sent.map_err(rav1d_err)?;
        let pic = match picture {
            Some(p) => p,
            None => match self.decoder.get_picture() {
                Ok(p) => p,
                Err(TryAgain) => return Err(CodecError::malformed(F, "an AV1 frame has no picture to show")),
                Err(e) => return Err(rav1d_err(e)),
            },
        };
        frame(&pic)
    }
}

#[cfg(feature = "avif")]
fn frame(pic: &rav1d::Picture) -> Result<Frame, CodecError> {
    use rav1d::{PixelLayout, PlanarImageComponent as P};
    let (width, height) = (pic.width(), pic.height());
    let sub = match pic.pixel_layout() {
        PixelLayout::I400 => None,
        PixelLayout::I420 => Some((1, 1)),
        PixelLayout::I422 => Some((1, 0)),
        PixelLayout::I444 => Some((0, 0)),
    };
    let (sx, sy) = sub.unwrap_or((0, 0));
    let (chroma_width, chroma_height) = ((width + sx) >> sx, (height + sy) >> sy);
    // 10- and 12-bit samples are stored as little-endian u16. `bit_depth` is the stream's bits
    // per component (10, 12), not the 8 or 16 its documentation says.
    let wide = pic.bit_depth() > 8;
    let plane = |c: P, w: u32, h: u32| -> Result<Vec<u16>, CodecError> {
        let (stride, data) = (pic.stride(c) as usize, pic.plane(c));
        let mut out = Vec::with_capacity(w as usize * h as usize);
        for y in 0..h as usize {
            let row = data.get(y * stride..).ok_or_else(|| CodecError::malformed(F, "AV1 plane is shorter than its size"))?;
            if wide {
                out.extend(row.as_chunks::<2>().0.iter().take(w as usize).map(|b| u16::from_le_bytes(*b)));
            } else {
                out.extend(row.iter().take(w as usize).map(|v| u16::from(*v)));
            }
        }
        if out.len() != w as usize * h as usize {
            return Err(CodecError::malformed(F, "AV1 plane is shorter than its size"));
        }
        Ok(out)
    };
    let y = plane(P::Y, width, height)?;
    let (u, v) = if sub.is_some() { (plane(P::U, chroma_width, chroma_height)?, plane(P::V, chroma_width, chroma_height)?) } else { Default::default() };
    Ok(Frame {
        width,
        height,
        sub,
        chroma_width,
        bits: pic.bits_per_component().map_or(8, |b| u32::from(b.0)),
        full_range: pic.color_range() == rav1d::pixel::YUVRange::Full,
        matrix: pic.matrix_coefficients(),
        y,
        u,
        v,
    })
}

#[cfg(feature = "avif")]
impl Frame {
    /// Luma (or a full-scale sample) 0..=1 for its range.
    fn luma(&self, v: u16) -> f32 {
        let scale = (1u32 << (self.bits - 8)) as f32;
        if self.full_range { f32::from(v) / ((1u32 << self.bits) - 1) as f32 } else { (f32::from(v) - 16.0 * scale) / (219.0 * scale) }
    }
    /// Chroma -0.5..=0.5 for its range.
    fn chroma(&self, v: u16) -> f32 {
        let scale = (1u32 << (self.bits - 8)) as f32;
        if self.full_range {
            (f32::from(v) - (1u32 << (self.bits - 1)) as f32) / ((1u32 << self.bits) - 1) as f32
        } else {
            (f32::from(v) - 128.0 * scale) / (224.0 * scale)
        }
    }
}

/// How decoded frames become pixels.
#[cfg(feature = "avif")]
#[derive(Clone, Copy)]
struct Output {
    /// 16 bits per sample (a 10- or 12-bit file) rather than 8.
    wide: bool,
    /// RGBA even without an alpha image (Save for Web's preview wants RGBA8 always).
    rgba: bool,
    premultiplied: bool,
    /// The container's CICP matrix, used when the AV1 stream leaves its own unspecified.
    matrix: Option<u16>,
}

/// `color` (and `alpha`, the same size) as RGB(A), nearest-sample chroma upsampling.
#[cfg(feature = "avif")]
fn to_image(color: &Frame, alpha: Option<&Frame>, out: Output) -> Result<Image, CodecError> {
    use crate::image::ChannelLayout;
    use rav1d::pixel::MatrixCoefficients as M;
    if alpha.is_some_and(|a| (a.width, a.height) != (color.width, color.height)) {
        return Err(CodecError::malformed(F, "the alpha image's size differs from the colour image's"));
    }
    // Luma weights (Kr, Kb) by matrix; unspecified and the BT.601 family use BT.601's.
    let matrix = match (color.matrix, out.matrix) {
        (M::Unspecified, Some(1)) => M::BT709,
        (M::Unspecified, Some(9)) => M::BT2020NonConstantLuminance,
        (M::Unspecified, Some(0)) => M::Identity,
        (m, _) => m,
    };
    let (kr, kb) = match matrix {
        M::BT709 => (0.2126, 0.0722),
        M::BT2020NonConstantLuminance => (0.2627, 0.0593),
        M::BT470M => (0.30, 0.11),
        M::ST240M => (0.212, 0.087),
        M::Identity | M::Unspecified | M::BT470BG | M::ST170M => (0.299, 0.114),
        other => return Err(CodecError::unsupported(F, format!("matrix coefficients {other:?}"))),
    };
    let kg = 1.0 - kr - kb;
    let (w, h) = (color.width as usize, color.height as usize);
    let channels = if alpha.is_some() || out.rgba { 4 } else { 3 };
    let mut px = Vec::with_capacity(w * h * channels);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let luma = color.luma(color.y[i]);
            let rgb = match color.sub {
                None => [luma; 3],
                Some((sx, sy)) => {
                    let c = (y >> sy) * color.chroma_width as usize + (x >> sx);
                    if matrix == M::Identity {
                        // GBR: Y holds green, U blue and V red, each a full-scale sample.
                        [color.luma(color.v[c]), luma, color.luma(color.u[c])]
                    } else {
                        let (cb, cr) = (color.chroma(color.u[c]), color.chroma(color.v[c]));
                        let r = luma + 2.0 * (1.0 - kr) * cr;
                        let b = luma + 2.0 * (1.0 - kb) * cb;
                        [r, (luma - kr * r - kb * b) / kg, b]
                    }
                }
            };
            let a = alpha.map_or(1.0, |a| a.luma(a.y[i]).clamp(0.0, 1.0));
            // A premultiplied file (`prem`) divides colour back out by alpha.
            let rgb = if out.premultiplied && a > 0.0 { rgb.map(|v| v / a) } else { rgb };
            px.extend_from_slice(&rgb);
            if channels == 4 {
                px.push(a);
            }
        }
    }
    let layout = if channels == 4 { ChannelLayout::Rgba } else { ChannelLayout::Rgb };
    if out.wide {
        let data: Vec<u16> = px.iter().map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16).collect();
        Image::from_u16(color.width, color.height, layout, &data)
    } else {
        Image::from_u8(color.width, color.height, layout, px.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect())
    }
}

/// Decodes `bytes` to straight RGBA8: the preview of a file [`crate::encode`] wrote.
#[cfg(feature = "avif")]
pub(crate) fn decode_preview(bytes: &[u8]) -> Result<Image, CodecError> {
    guarded(|| {
        let avif = avif_parse::read_avif(&mut &bytes[..]).map_err(|e| CodecError::malformed(F, e))?;
        let limits = crate::options::Limits::default();
        let color = Av1::new(&limits)?.decode(&avif.primary_item)?;
        let alpha = avif.alpha_item.as_deref().map(|a| Av1::new(&limits)?.decode(a)).transpose()?;
        to_image(&color, alpha.as_ref(), Output { wide: false, rgba: true, premultiplied: avif.premultiplied_alpha, matrix: None })
    })
}

/// The colour description an image gets from its container, and HDR (PQ, HLG) turned into linear
/// light in 32-bit float as a HEIF's is: no ICC curve describes those transfers.
#[cfg(feature = "avif")]
fn tag(mut img: Image, colour: &super::avif_boxes::Colour, limits: &crate::options::Limits) -> Result<Image, CodecError> {
    img.icc = colour.icc.clone();
    img.cicp = colour.cicp;
    match img.cicp.filter(|c| c.is_hdr() && colour.icc.is_none()) {
        Some(c) => {
            limits.check(img.width(), img.height(), img.layout(), crate::image::SampleType::F32)?;
            crate::cicp::linearize(img, c)
        }
        None => Ok(img),
    }
}

/// The file's still image for File › Open: its primary item, or the first frame of a sequence
/// without one, with its colour description and orientation applied.
#[cfg(feature = "avif")]
pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Image, CodecError> {
    guarded(|| {
        let limits = &opts.limits;
        let avif = match avif_parse::read_avif(&mut &bytes[..]) {
            Ok(a) => a,
            // A sequence-only `avis` has no primary item: open its first frame.
            Err(e) => {
                return match super::avif_boxes::sequence(bytes, super::gif_frames::MAX_GIF_FRAMES)? {
                    Some(_) => decode_frames_limited(bytes, opts, 1)?.into_iter().next().map(|f| f.image).ok_or_else(|| CodecError::malformed(F, e)),
                    None => Err(CodecError::malformed(F, e)),
                };
            }
        };
        let props = super::avif_boxes::primary_props(bytes)?;
        let meta = avif.primary_item_metadata().map_err(|e| CodecError::malformed(F, e))?;
        let (w, h) = (meta.max_frame_width.get(), meta.max_frame_height.get());
        let wide = meta.bit_depth > 8;
        let layout = if avif.alpha_item.is_some() { crate::image::ChannelLayout::Rgba } else { crate::image::ChannelLayout::Rgb };
        limits.check(w, h, layout, if wide { crate::image::SampleType::U16 } else { crate::image::SampleType::U8 })?;
        let color = Av1::new(limits)?.decode(&avif.primary_item)?;
        let alpha = avif.alpha_item.as_deref().map(|a| Av1::new(limits)?.decode(a)).transpose()?;
        let out = Output { wide, rgba: false, premultiplied: avif.premultiplied_alpha, matrix: props.colour.cicp.map(|c| c.matrix) };
        let img = to_image(&color, alpha.as_ref(), out)?;
        let img = orient(img, props.rotation, props.mirror)?;
        tag(img, &props.colour, limits)
    })
}

/// Applies `irot` (anticlockwise quarter turns) and then `imir`.
#[cfg(feature = "avif")]
fn orient(img: Image, quarter_turns: u8, mirror: Option<u8>) -> Result<Image, CodecError> {
    if quarter_turns == 0 && mirror.is_none() {
        return Ok(img);
    }
    let (w, h) = (img.width() as usize, img.height() as usize);
    let bpp = img.layout().channels() * img.sample_type().bytes();
    let src = img.data();
    let (ow, oh) = if quarter_turns % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; src.len()];
    for oy in 0..oh {
        for ox in 0..ow {
            // The mirror applies after the rotation, so undo it first on the output position.
            let (mx, my) = match mirror {
                Some(0) => (ox, oh - 1 - oy),
                Some(_) => (ow - 1 - ox, oy),
                None => (ox, oy),
            };
            // Where that rotated position came from: an anticlockwise turn maps (x, y) to (y, w-1-x).
            let (sx, sy) = match quarter_turns {
                1 => (w - 1 - my, mx),
                2 => (w - 1 - mx, h - 1 - my),
                3 => (my, h - 1 - mx),
                _ => (mx, my),
            };
            let (s, d) = ((sy * w + sx) * bpp, (oy * ow + ox) * bpp);
            out[d..d + bpp].copy_from_slice(&src[s..s + bpp]);
        }
    }
    let mut turned = Image::from_raw(ow as u32, oh as u32, img.layout(), img.sample_type(), out)?;
    turned.icc = img.icc;
    turned.cicp = img.cicp;
    turned.meta = img.meta;
    turned.warnings = img.warnings;
    Ok(turned)
}

/// Whether `bytes` hold an image sequence (an animated AVIF) whose track tables read cleanly.
#[cfg(feature = "avif")]
pub(crate) fn is_sequence(bytes: &[u8]) -> bool {
    matches!(super::avif_boxes::sequence(bytes, super::gif_frames::MAX_GIF_FRAMES), Ok(Some(_)))
}

/// Every frame of an animated AVIF, each a full picture with its duration; a still AVIF yields
/// its one image. The frame count and total decoded size are checked against `opts.limits`; a
/// frame that fails to decode after the first ends the animation there with a
/// [`crate::image::DecodeWarning::Truncated`] on the first frame, as an animated WebP's does.
#[cfg(feature = "avif")]
pub(crate) fn decode_frames(bytes: &[u8], opts: &DecodeOptions) -> Result<Vec<super::gif_frames::AnimationFrame>, CodecError> {
    decode_frames_limited(bytes, opts, super::gif_frames::MAX_GIF_FRAMES)
}

#[cfg(feature = "avif")]
fn decode_frames_limited(bytes: &[u8], opts: &DecodeOptions, max_frames: usize) -> Result<Vec<super::gif_frames::AnimationFrame>, CodecError> {
    use super::gif_frames::AnimationFrame;
    use crate::image::{ChannelLayout, DecodeWarning, SampleType};
    guarded(|| {
        let limits = &opts.limits;
        let Some(seq) = super::avif_boxes::sequence(bytes, max_frames.max(super::gif_frames::MAX_GIF_FRAMES))? else {
            return Ok(vec![AnimationFrame { image: decode(bytes, opts)?, delay_ms: 0 }]);
        };
        let layout = if seq.alpha.is_some() { ChannelLayout::Rgba } else { ChannelLayout::Rgb };
        limits.check(seq.width.max(1), seq.height.max(1), layout, SampleType::U16)?;
        let (mut colour_dec, mut alpha_dec) = (Av1::new(limits)?, Av1::new(limits)?);
        let mut out: Vec<AnimationFrame> = Vec::with_capacity(seq.color.len().min(max_frames).min(1024));
        let mut damaged = false;
        let mut wide = None;
        for (i, sample) in seq.color.iter().enumerate().take(max_frames) {
            let decoded = (|| {
                let color = colour_dec.decode(sample.data)?;
                let alpha = seq.alpha.as_ref().and_then(|a| a.get(i)).map(|a| alpha_dec.decode(a.data)).transpose()?;
                // Every frame keeps the first one's depth, so the layers share one format.
                let wide = *wide.get_or_insert(color.bits > 8);
                let size = (color.width as u64) * (color.height as u64) * (layout.channels() as u64) * if wide { 2 } else { 1 };
                if size.saturating_mul(out.len() as u64 + 1) > limits.max_alloc {
                    return Err(CodecError::LimitExceeded(format!(
                        "{} frames of {}x{} exceed max_alloc {}",
                        out.len() + 1,
                        color.width,
                        color.height,
                        limits.max_alloc
                    )));
                }
                let matrix = seq.colour.cicp.map(|c| c.matrix);
                let img = to_image(&color, alpha.as_ref(), Output { wide, rgba: seq.alpha.is_some(), premultiplied: false, matrix })?;
                tag(img, &seq.colour, limits)
            })();
            match decoded {
                Ok(image) => out.push(AnimationFrame { image, delay_ms: sample.duration_ms }),
                Err(e @ CodecError::LimitExceeded(_)) => return Err(e),
                Err(e) if out.is_empty() => return Err(e),
                Err(_) => {
                    damaged = true;
                    break;
                }
            }
        }
        if damaged && let Some(first) = out.first_mut() {
            first.image.warnings.push(DecodeWarning::Truncated { format: F });
        }
        Ok(out)
    })
}

#[cfg(all(test, feature = "avif"))]
mod tests {
    use crate::{ChannelLayout, EncodeOptions, Format, Image, encode};

    /// A gradient with a translucent band, through our writer and back: close at high quality,
    /// visibly lossy at low quality, alpha kept.
    #[test]
    fn our_avif_round_trips_with_its_artefacts_and_alpha() {
        let (w, h) = (96u32, 64u32);
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let a = if (16..32).contains(&y) { 128 } else { 255 };
                data.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, ((x ^ y) * 3) as u8, a]);
            }
        }
        let src = Image::from_u8(w, h, ChannelLayout::Rgba, data.clone()).unwrap();
        let mse = |q: u8| {
            let bytes = encode(&src, Format::Avif, &EncodeOptions { jpeg_quality: q, ..Default::default() }).unwrap();
            let out = super::decode_preview(&bytes).unwrap();
            assert_eq!((out.width(), out.height(), out.layout()), (w, h, ChannelLayout::Rgba));
            let px = out.data();
            let alpha_ok = px.as_chunks::<4>().0.iter().zip(data.as_chunks::<4>().0).all(|(a, b)| a[3].abs_diff(b[3]) <= 8);
            assert!(alpha_ok, "alpha survives at quality {q}");
            px.iter().zip(&data).map(|(a, b)| f64::from(a.abs_diff(*b)).powi(2)).sum::<f64>() / px.len() as f64
        };
        let (hi, lo) = (mse(95), mse(5));
        assert!(hi < 10.0, "quality 95 is close to the source: MSE {hi}");
        assert!(lo > hi * 4.0, "quality 5 shows its artefacts: MSE {lo} vs {hi}");
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(super::decode_preview(b"\0\0\0\x18ftypavif").is_err());
        assert!(super::decode_preview(&[]).is_err());
    }
}
