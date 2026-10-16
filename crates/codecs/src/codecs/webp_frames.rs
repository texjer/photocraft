//! Every frame of an animated WebP. [`crate::decode`] keeps the first frame only; this walks the
//! whole animation for callers that can hold it (frame animations, video layers).
//!
//! `image-webp` blends each frame onto the canvas and applies its disposal, so every returned
//! frame is a full, self-contained picture, as [`super::gif_frames`] gives for a GIF.

use crate::Format;
use crate::codecs::gif_frames::{AnimationFrame, MAX_GIF_FRAMES};
use crate::error::CodecError;
use crate::image::{DecodeWarning, Image};
use crate::options::{DecodeOptions, alloc_zeroed};

/// Decodes every frame of a WebP (a still one yields one), each a full picture of the canvas
/// with its duration.
///
/// The first frame must decode. If a later frame is damaged the frames before it are returned
/// and the first one carries [`DecodeWarning::Truncated`]. The frame count ([`MAX_GIF_FRAMES`])
/// and the total decoded bytes are checked against `opts.limits` before each frame is kept.
pub fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Vec<AnimationFrame>, CodecError> {
    let limits = &opts.limits;
    let (mut dec, w, h, layout) = super::webp::open(bytes, limits)?;
    if !dec.is_animated() {
        let image = super::webp::decode(bytes, limits)?;
        return Ok(vec![AnimationFrame { image, delay_ms: 0 }]);
    }
    let icc = dec.icc_profile().ok().flatten();
    let size = dec.output_buffer_size().ok_or_else(|| CodecError::malformed(Format::WebP, "image too large"))?;
    let n = dec.num_frames() as usize;
    if n > MAX_GIF_FRAMES {
        return Err(CodecError::LimitExceeded(format!("more than {MAX_GIF_FRAMES} frames")));
    }
    let mut out: Vec<AnimationFrame> = Vec::with_capacity(n.min(1024));
    let mut damaged = false;
    for _ in 0..n {
        if (size as u64).saturating_mul(out.len() as u64 + 1) > limits.max_alloc {
            return Err(CodecError::LimitExceeded(format!("{} frames of {w}x{h} exceed max_alloc {}", out.len() + 1, limits.max_alloc)));
        }
        let mut buf = alloc_zeroed::<u8>(size)?;
        let delay_ms = match dec.read_frame(&mut buf) {
            Ok(d) => d,
            Err(e) if out.is_empty() => return Err(CodecError::malformed(Format::WebP, e)),
            Err(_) => {
                damaged = true;
                break;
            }
        };
        out.push(AnimationFrame { image: Image::from_u8(w, h, layout, buf)?, delay_ms });
    }
    if let Some(first) = out.first_mut() {
        first.image.icc = icc;
        if damaged {
            first.image.warnings.push(DecodeWarning::Truncated { format: Format::WebP });
        }
    }
    Ok(out)
}
