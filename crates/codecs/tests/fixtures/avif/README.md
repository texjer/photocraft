# AVIF fixtures

Made for these tests with libavif's `avifenc` 1.4.2 (aom 3.15.1) from flat-colour images drawn
with ImageMagick, so a writer other than ours produced them. No third-party pictures; they are
released under the project's licence.

| File | Made with | Holds |
|---|---|---|
| `anim-rgb-3-frames.avif` | `avifenc -q 90 --timescale 1000 --duration 100 red.png --duration 250 green.png --duration 400 blue.png` | 16x12, 8-bit 4:4:4, frames of solid red, green, blue showing 100, 250 and 400 ms |
| `anim-alpha-10bit.avif` | `avifenc -q 100 -d 10 --timescale 30 --duration 3 a1.png --duration 6 a2.png` | 8x6, 10-bit, an alpha track: red at 50 % opacity for 100 ms, then opaque green for 200 ms |
| `still-irot1.avif` | `avifenc -l --irot 1 still.png` | 6x4 black with a red pixel top-left and a green one top-right, stored with a quarter turn anticlockwise (`irot` 1), so it shows 4x6 with green top-left and red bottom-left |
