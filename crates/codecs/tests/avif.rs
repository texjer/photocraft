//! AVIF read, against files libavif's `avifenc` wrote (`fixtures/avif/README.md`): a still image's
//! orientation, and an animated AVIF's frames, durations, alpha track and bit depth.
#![cfg(feature = "avif")]

use photocraft_codecs::{ChannelLayout, CodecError, DecodeOptions, Format, Limits, SampleType, decode, decode_avif_frames, decode_frames, detect};

const ANIM: &[u8] = include_bytes!("fixtures/avif/anim-rgb-3-frames.avif");
const ALPHA_10: &[u8] = include_bytes!("fixtures/avif/anim-alpha-10bit.avif");
const IROT: &[u8] = include_bytes!("fixtures/avif/still-irot1.avif");

fn rgb8(px: &[u8]) -> [u8; 3] {
    [px[0], px[1], px[2]]
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 6)
}

#[test]
fn an_animated_avif_decodes_every_frame_with_its_duration() {
    assert_eq!(detect(ANIM), Some(Format::Avif));
    let frames = decode_frames(ANIM, &DecodeOptions::default()).expect("AVIF has frames").unwrap();
    let delays: Vec<u32> = frames.iter().map(|f| f.delay_ms).collect();
    assert_eq!(delays, [100, 250, 400]);
    for (f, want) in frames.iter().zip([[255, 0, 0], [0, 255, 0], [0, 0, 255]]) {
        assert_eq!((f.image.width(), f.image.height(), f.image.layout()), (16, 12, ChannelLayout::Rgb));
        let px = f.image.data();
        assert!(near(rgb8(px), want), "{:?} vs {want:?}", rgb8(px));
    }
}

#[test]
fn opening_an_animated_avif_as_a_still_shows_its_first_frame() {
    let img = decode(ANIM).unwrap();
    assert_eq!((img.width(), img.height()), (16, 12));
    assert!(near(rgb8(img.data()), [255, 0, 0]));
}

#[test]
fn an_alpha_track_and_ten_bits_open_as_sixteen_bit_rgba() {
    let frames = decode_avif_frames(ALPHA_10, &DecodeOptions::default()).unwrap();
    assert_eq!(frames.iter().map(|f| f.delay_ms).collect::<Vec<_>>(), [100, 200]);
    let first = &frames[0].image;
    assert_eq!((first.layout(), first.sample_type()), (ChannelLayout::Rgba, SampleType::U16));
    let px: Vec<u16> = first.data().as_chunks::<2>().0.iter().map(|b| u16::from_ne_bytes(*b)).collect();
    assert!(px[0] > 64000 && px[1] < 1500 && px[2] < 1500, "red: {:?}", &px[..4]);
    assert!(px[3].abs_diff(32768) < 1200, "half transparent: {}", px[3]);
    let second: Vec<u16> = frames[1].image.data().as_chunks::<2>().0.iter().map(|b| u16::from_ne_bytes(*b)).collect();
    assert!(second[1] > 64000 && second[3] == 65535, "opaque green: {:?}", &second[..4]);
}

#[test]
fn irot_turns_the_still_image_upright() {
    let img = decode(IROT).unwrap();
    assert_eq!((img.width(), img.height()), (4, 6));
    let px = img.data();
    let at = |x: usize, y: usize| rgb8(&px[(y * 4 + x) * 3..]);
    assert!(near(at(0, 0), [0, 255, 0]), "green top-left: {:?}", at(0, 0));
    assert!(near(at(0, 5), [255, 0, 0]), "red bottom-left: {:?}", at(0, 5));
    assert!(near(at(3, 0), [0, 0, 0]));
}

#[test]
fn a_frame_count_over_the_allocation_limit_is_refused() {
    let opts = DecodeOptions { limits: Limits { max_alloc: 16 * 12 * 3 * 2, ..Limits::default() }, ..DecodeOptions::default() };
    assert!(matches!(decode_avif_frames(ANIM, &opts), Err(CodecError::LimitExceeded(_))));
}

#[test]
fn cut_and_damaged_files_are_errors_not_panics() {
    for n in [0, 12, 40, 200, 600, ANIM.len() - 1] {
        let _ = decode(&ANIM[..n]);
        let _ = decode_avif_frames(&ANIM[..n], &DecodeOptions::default());
    }
    // Every byte of the sample table flipped in turn.
    for i in 0..ANIM.len() {
        let mut b = ANIM.to_vec();
        b[i] ^= 0xff;
        let _ = decode_avif_frames(&b, &DecodeOptions::default());
    }
}

/// Our animated AVIF writer, read back by our reader: every frame, its duration and its alpha.
#[test]
fn an_animated_avif_we_write_reads_back_frame_by_frame() {
    use photocraft_codecs::web::{AvifFrame, encode_avif_animated};
    let (w, h) = (24u32, 16u32);
    let solid = |c: [u8; 4], delay_ms| AvifFrame { rgba: c.repeat((w * h) as usize), delay_ms };
    let frames = [solid([255, 0, 0, 255], 80), solid([0, 200, 0, 255], 120), solid([0, 0, 255, 128], 0)];
    let bytes = encode_avif_animated(w, h, &frames, 90, 10, 0).unwrap();
    assert_eq!(detect(&bytes), Some(Format::Avif));
    let back = decode_avif_frames(&bytes, &DecodeOptions::default()).unwrap();
    assert_eq!(back.iter().map(|f| f.delay_ms).collect::<Vec<_>>(), [80, 120, 100], "a 0 delay is written as 100 ms");
    for (f, want) in back.iter().zip(&frames) {
        assert_eq!((f.image.width(), f.image.height(), f.image.layout()), (w, h, ChannelLayout::Rgba));
        let (got, want) = (&f.image.data()[..4], &want.rgba[..4]);
        assert!(got.iter().zip(want).all(|(a, b)| a.abs_diff(*b) <= 8), "{got:?} vs {want:?}");
    }
    // The still image is the first frame, for readers that don't play sequences.
    assert!(near(rgb8(decode(&bytes).unwrap().data()), [255, 0, 0]));
}

#[test]
fn an_opaque_animation_has_no_alpha_track() {
    use photocraft_codecs::web::{AvifFrame, encode_avif_animated};
    let frames: Vec<AvifFrame> = (0..4).map(|i| AvifFrame { rgba: [i * 60, 0, 0, 255].repeat(256), delay_ms: 50 }).collect();
    let bytes = encode_avif_animated(16, 16, &frames, 70, 10, 2).unwrap();
    let back = decode_avif_frames(&bytes, &DecodeOptions::default()).unwrap();
    assert_eq!(back.len(), 4);
    assert!(back.iter().all(|f| f.image.layout() == ChannelLayout::Rgb));
    assert!(encode_avif_animated(16, 16, &[], 70, 10, 0).is_err());
    assert!(encode_avif_animated(16, 16, &[AvifFrame { rgba: vec![0; 3], delay_ms: 0 }], 70, 10, 0).is_err());
    let small: Vec<AvifFrame> = (0..2).map(|_| AvifFrame { rgba: vec![0; 8 * 8 * 4], delay_ms: 50 }).collect();
    assert!(encode_avif_animated(8, 8, &small, 70, 10, 0).unwrap_err().to_string().contains("16"), "too small for rav1e");
}
