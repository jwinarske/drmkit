// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The image type, the blend, and the PNG writer.

use crate::image::{Image, src_over, unpremultiply};
use crate::{CaptureError, write_png};

/// Build a premultiplied `0xAARRGGBB`.
const fn argb(a: u8, r: u8, g: u8, b: u8) -> u32 {
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

#[test]
fn a_default_image_is_empty() {
    let image = Image::default();
    assert_eq!(image.width(), 0);
    assert_eq!(image.height(), 0);
    assert!(image.is_empty());
    assert!(image.pixels().is_empty());
}

#[test]
fn a_zero_dimension_stays_empty() {
    // Not a 0x100 picture with no pixels -- an empty one. Otherwise every
    // consumer has to check both dimensions rather than asking once.
    for (w, h) in [(0, 16), (16, 0), (0, 0)] {
        let image = Image::new(w, h);
        assert!(image.is_empty(), "{w}x{h} must be empty");
        assert!(image.pixels().is_empty());
    }
}

#[test]
fn dimensions_and_stride() {
    let image = Image::new(4, 3);
    assert_eq!(image.width(), 4);
    assert_eq!(image.height(), 3);
    assert_eq!(image.stride_bytes(), 16);
    assert_eq!(image.pixels().len(), 12);
    assert!(image.pixels().iter().all(|p| *p == 0), "starts transparent");
}

#[test]
fn pixels_are_writable_and_kept() {
    let mut image = Image::new(16, 16);
    image.pixels_mut()[0] = 0xff00_00ff;
    let moved = image;
    assert_eq!(moved.pixels()[0], 0xff00_00ff);
    assert_eq!(moved.width(), 16);
}

// --- the blend -------------------------------------------------------------

#[test]
fn an_opaque_source_replaces_the_destination() {
    let dst = argb(255, 10, 20, 30);
    let src = argb(255, 200, 100, 50);
    assert_eq!(src_over(dst, src), src);
}

#[test]
fn a_transparent_source_leaves_the_destination() {
    let dst = argb(255, 10, 20, 30);
    assert_eq!(src_over(dst, 0), dst);
}

#[test]
fn half_alpha_over_black_is_the_source() {
    // Premultiplied: a half-covered white pixel is already 0x80808080. Over
    // transparent black it stays exactly itself -- nothing is added.
    let src = argb(128, 128, 128, 128);
    assert_eq!(src_over(0, src), src);
}

#[test]
fn compositing_is_exact_at_the_endpoints() {
    // The classic failure of a truncating divide-by-255 is that opaque over
    // opaque comes out one short. Check the corners where that shows.
    let white = argb(255, 255, 255, 255);
    let black = argb(255, 0, 0, 0);
    assert_eq!(src_over(black, white), white);
    assert_eq!(src_over(white, black), black);

    // A just-barely-translucent source over white must not lose the last
    // step of the destination.
    let almost = argb(254, 0, 0, 0);
    let out = src_over(white, almost);
    assert_eq!(out >> 24, 255, "alpha saturates back to opaque");
    assert_eq!(out & 0xff, 1, "one unit of white survives, not zero");
}

#[test]
fn the_blend_never_exceeds_full_coverage() {
    // Premultiplied SRC_OVER must not produce a channel above its own alpha,
    // and must not overflow a byte, for any pair of representable inputs.
    for sa in [0u8, 1, 63, 127, 128, 200, 254, 255] {
        for da in [0u8, 1, 63, 127, 128, 200, 254, 255] {
            // Channel values at their premultiplied maximum: equal to alpha.
            let src = argb(sa, sa, sa, sa);
            let dst = argb(da, da, da, da);
            let out = src_over(dst, src);
            let a = out >> 24;
            for shift in [16, 8, 0] {
                let c = (out >> shift) & 0xff;
                assert!(c <= a, "sa={sa} da={da}: channel {c} exceeds alpha {a}");
                assert!(c <= 255);
            }
        }
    }
}

#[test]
fn stacking_is_ordered() {
    // Bottom-up: the last plane composited is the one on top.
    let red = argb(255, 255, 0, 0);
    let blue = argb(255, 0, 0, 255);
    assert_eq!(src_over(src_over(0, red), blue), blue);
    assert_eq!(src_over(src_over(0, blue), red), red);
}

// --- straight alpha --------------------------------------------------------

#[test]
fn opaque_pixels_round_trip_exactly() {
    // A screenshot is opaque almost everywhere, so this is the case that has
    // to be exact rather than close.
    for c in 0..=255u8 {
        assert_eq!(unpremultiply(argb(255, c, c, c)), [c, c, c, 255]);
    }
    assert_eq!(unpremultiply(argb(255, 12, 34, 56)), [12, 34, 56, 255]);
}

#[test]
fn a_fully_transparent_pixel_has_no_colour_to_recover() {
    assert_eq!(unpremultiply(0), [0, 0, 0, 0]);
    // Even if a producer left colour behind in a zero-alpha pixel, there is
    // nothing to divide by and nothing that pixel can mean.
    assert_eq!(unpremultiply(argb(0, 200, 100, 50)), [0, 0, 0, 0]);
}

#[test]
fn half_covered_white_comes_back_white() {
    // 0x80 premultiplied white is a half-covered white pixel; straight, it is
    // white at half alpha.
    let [r, g, b, a] = unpremultiply(argb(128, 128, 128, 128));
    assert_eq!(a, 128);
    for c in [r, g, b] {
        assert_eq!(c, 255, "half of white, un-premultiplied, is white");
    }
}

#[test]
fn a_channel_above_its_alpha_is_clamped_not_wrapped() {
    // Nothing stops a producer writing a pixel whose colour exceeds its
    // coverage. The division then lands above 255, and the answer has to be
    // "as bright as it goes" rather than a wrapped byte.
    let [r, g, b, a] = unpremultiply(argb(64, 200, 128, 65));
    assert_eq!(a, 64);
    assert_eq!(r, 255, "200/64 saturates");
    assert_eq!(g, 255, "128/64 saturates");
    assert_eq!(b, 255, "65/64 saturates");
}

#[test]
fn un_premultiplying_is_monotonic() {
    // More premultiplied colour at the same coverage can never mean less
    // straight colour.
    for a in [1u8, 7, 64, 128, 254, 255] {
        let mut previous = 0u8;
        for c in 0..=a {
            let [r, _, _, _] = unpremultiply(argb(a, c, 0, 0));
            assert!(r >= previous, "a={a} c={c}: {r} went below {previous}");
            previous = r;
        }
    }
}

// --- PNG -------------------------------------------------------------------

/// A scratch path that cleans up after itself.
struct TempPng(std::path::PathBuf);

impl TempPng {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("drmkit_capture_{name}_{}.png", std::process::id()));
        let _ = std::fs::remove_file(&path);
        Self(path)
    }
}

impl Drop for TempPng {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn writing_an_empty_image_is_refused() {
    // A zero-by-zero PNG is a file no decoder accepts. Better to fail at the
    // one place that knows why than to hand someone a broken file.
    let temp = TempPng::new("empty");
    assert!(matches!(
        write_png(&Image::default(), &temp.0),
        Err(CaptureError::EmptyImage)
    ));
    assert!(!temp.0.exists(), "nothing should have been created");
}

#[test]
fn a_written_png_starts_with_the_png_signature() {
    let mut image = Image::new(8, 8);
    let width = image.width();
    for (index, pixel) in image.pixels_mut().iter_mut().enumerate() {
        let index = u32::try_from(index).expect("small image");
        let (x, y) = (index % width, index / width);
        // 2x2 checkerboard, opaque, so the premultiply is the identity.
        *pixel = if ((x / 2) + (y / 2)) % 2 == 0 {
            argb(255, 255, 255, 255)
        } else {
            argb(255, 0, 0, 0)
        };
    }

    let temp = TempPng::new("signature");
    write_png(&image, &temp.0).expect("write");

    let bytes = std::fs::read(&temp.0).expect("read back");
    assert!(bytes.len() > 8);
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
        "PNG signature"
    );
}

#[test]
fn a_written_png_decodes_back_to_what_went_in() {
    // The C++ suite checks the magic bytes and stops. Decoding it is what
    // says the pixels survived -- including that the rows went out in the
    // right order, which a signature check cannot see.
    let mut image = Image::new(4, 3);
    for (index, pixel) in image.pixels_mut().iter_mut().enumerate() {
        let i = u8::try_from(index).expect("small");
        // Every pixel distinct, so a transposed or reversed row shows up.
        *pixel = argb(255, i * 20, 255 - (i * 20), i.wrapping_mul(37));
    }
    let expected: Vec<[u8; 4]> = image.pixels().iter().map(|p| unpremultiply(*p)).collect();

    let temp = TempPng::new("roundtrip");
    write_png(&image, &temp.0).expect("write");

    let file = std::io::BufReader::new(std::fs::File::open(&temp.0).expect("open"));
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().expect("header");
    let info = reader.info();
    assert_eq!((info.width, info.height), (4, 3));
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);

    let mut buffer = vec![0; reader.output_buffer_size().expect("size")];
    let output = reader.next_frame(&mut buffer).expect("decode");
    let got_rows = &buffer[..output.buffer_size()];

    for (index, want) in expected.iter().enumerate() {
        let got = &got_rows[index * 4..index * 4 + 4];
        assert_eq!(got, want, "pixel {index}");
    }
}

#[test]
fn translucency_survives_the_round_trip_to_the_precision_it_can() {
    // Straight alpha on disk means a translucent pixel comes back through
    // premultiply -> un-premultiply. That loses precision at low coverage and
    // must not lose it at high.
    let mut image = Image::new(4, 1);
    let alphas = [255u8, 200, 128, 8];
    for (pixel, a) in image.pixels_mut().iter_mut().zip(alphas) {
        // A white pixel at each coverage, premultiplied.
        *pixel = argb(a, a, a, a);
    }

    let temp = TempPng::new("alpha");
    write_png(&image, &temp.0).expect("write");

    let file = std::io::BufReader::new(std::fs::File::open(&temp.0).expect("open"));
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().expect("header");
    let mut buffer = vec![0; reader.output_buffer_size().expect("size")];
    let output = reader.next_frame(&mut buffer).expect("decode");
    let got_rows = &buffer[..output.buffer_size()];

    for (index, a) in alphas.iter().enumerate() {
        let got = &got_rows[index * 4..index * 4 + 4];
        assert_eq!(got[3], *a, "alpha at {index} is carried exactly");
        for channel in &got[..3] {
            assert_eq!(*channel, 255, "white stays white at alpha {a}");
        }
    }
}

// --- placement -------------------------------------------------------------

use crate::snapshot::{Blit, SourceFormat, blit};

/// A framebuffer of one solid premultiplied colour.
fn solid(width: u32, height: u32, pixel: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        bytes.extend_from_slice(&pixel.to_ne_bytes());
    }
    bytes
}

/// A `Blit` over `bytes` with a tight pitch, landing 1:1 at `(x, y)`.
fn at(bytes: &[u8], width: u32, height: u32, x: i32, y: i32) -> Blit<'_> {
    Blit {
        bytes,
        width,
        height,
        pitch: width * 4,
        format: SourceFormat::Argb8888,
        crtc_x: x,
        crtc_y: y,
        crtc_w: 0,
        crtc_h: 0,
    }
}

fn pixel_at(image: &Image, x: u32, y: u32) -> u32 {
    image.pixels()[(y * image.width() + x) as usize]
}

#[test]
fn a_plane_lands_where_its_crtc_offset_says() {
    let red = argb(255, 255, 0, 0);
    let bytes = solid(2, 2, red);
    let mut image = Image::new(8, 8);
    blit(&mut image, &at(&bytes, 2, 2, 3, 4));

    assert_eq!(pixel_at(&image, 3, 4), red);
    assert_eq!(pixel_at(&image, 4, 5), red);
    assert_eq!(pixel_at(&image, 2, 4), 0, "nothing left of it");
    assert_eq!(pixel_at(&image, 5, 4), 0, "nothing right of it");
    assert_eq!(pixel_at(&image, 3, 6), 0, "nothing below it");
}

#[test]
fn a_zero_destination_size_means_the_framebuffer_size() {
    // An unset CRTC_W/CRTC_H is how the kernel spells "1:1". Reading it as
    // literally zero would drop the plane entirely.
    let white = argb(255, 255, 255, 255);
    let bytes = solid(4, 4, white);
    let mut image = Image::new(4, 4);
    blit(&mut image, &at(&bytes, 4, 4, 0, 0));
    assert!(image.pixels().iter().all(|p| *p == white));
}

#[test]
fn a_plane_hanging_off_the_top_left_is_clipped_not_wrapped() {
    // CRTC_X and CRTC_Y are signed for this reason. Treating them as unsigned
    // puts the plane at four billion and drops it -- or worse, indexes with a
    // wrapped value.
    let green = argb(255, 0, 255, 0);
    let bytes = solid(4, 4, green);
    let mut image = Image::new(4, 4);
    blit(&mut image, &at(&bytes, 4, 4, -2, -2));

    assert_eq!(pixel_at(&image, 0, 0), green, "the visible quarter lands");
    assert_eq!(pixel_at(&image, 1, 1), green);
    assert_eq!(pixel_at(&image, 2, 2), 0, "the rest is off-screen");
    assert_eq!(pixel_at(&image, 3, 3), 0);
}

#[test]
fn a_plane_hanging_off_the_bottom_right_is_clipped() {
    let blue = argb(255, 0, 0, 255);
    let bytes = solid(4, 4, blue);
    let mut image = Image::new(4, 4);
    blit(&mut image, &at(&bytes, 4, 4, 2, 2));

    assert_eq!(pixel_at(&image, 3, 3), blue);
    assert_eq!(pixel_at(&image, 2, 2), blue);
    assert_eq!(pixel_at(&image, 1, 1), 0);
}

#[test]
fn a_plane_entirely_off_screen_contributes_nothing() {
    let bytes = solid(4, 4, argb(255, 255, 255, 255));
    let mut image = Image::new(4, 4);
    blit(&mut image, &at(&bytes, 4, 4, 100, 100));
    blit(&mut image, &at(&bytes, 4, 4, -100, -100));
    assert!(image.pixels().iter().all(|p| *p == 0));
}

#[test]
fn xrgb_is_opaque_whatever_its_top_byte_says() {
    // XRGB8888's alpha byte is undefined. A buffer that happens to hold zero
    // there would vanish entirely if SRC_OVER believed it.
    let bytes = solid(2, 2, 0x0000_00ff); // alpha byte zero, blue set
    let mut image = Image::new(2, 2);
    let mut src = at(&bytes, 2, 2, 0, 0);
    src.format = SourceFormat::Xrgb8888;
    blit(&mut image, &src);

    assert_eq!(
        pixel_at(&image, 0, 0),
        0xff00_00ff,
        "the plane is opaque blue, not invisible"
    );
}

#[test]
fn argb_with_a_zero_alpha_really_is_invisible() {
    // The converse of the above: on a plane that *does* have alpha, a zero
    // top byte means transparent and must be honoured.
    let bytes = solid(2, 2, 0x0000_00ff);
    let mut image = Image::new(2, 2);
    blit(&mut image, &at(&bytes, 2, 2, 0, 0));
    assert!(image.pixels().iter().all(|p| *p == 0));
}

#[test]
fn planes_stack_in_the_order_they_are_blitted() {
    let red = argb(255, 255, 0, 0);
    let blue = argb(255, 0, 0, 255);
    let under = solid(4, 4, red);
    let over = solid(2, 2, blue);

    let mut image = Image::new(4, 4);
    blit(&mut image, &at(&under, 4, 4, 0, 0));
    blit(&mut image, &at(&over, 2, 2, 0, 0));

    assert_eq!(pixel_at(&image, 0, 0), blue, "the later plane is on top");
    assert_eq!(pixel_at(&image, 3, 3), red, "and does not cover everything");
}

#[test]
fn a_translucent_plane_lets_the_one_beneath_through() {
    let opaque_red = argb(255, 255, 0, 0);
    let half_white = argb(128, 128, 128, 128);
    let under = solid(2, 2, opaque_red);
    let over = solid(2, 2, half_white);

    let mut image = Image::new(2, 2);
    blit(&mut image, &at(&under, 2, 2, 0, 0));
    blit(&mut image, &at(&over, 2, 2, 0, 0));

    // Premultiplied, half-white over full red is 128 (the white's own
    // contribution) plus 127 (the red showing through) in red, and 128 in
    // green and blue from the white alone. Straight, that is (255, 128, 128):
    // a pink. Reading the premultiplied channels as if they were straight is
    // the mistake this pins.
    let out = pixel_at(&image, 0, 0);
    assert_eq!(out >> 24, 255, "opaque underneath means an opaque result");
    assert_eq!(out, argb(255, 255, 128, 128));
    assert_eq!(
        unpremultiply(out),
        [255, 128, 128, 255],
        "which is a pink, not a white and not a red"
    );
}

#[test]
fn a_pitch_larger_than_the_width_skips_the_padding() {
    // Scanout strides are aligned, so pitch routinely exceeds width * 4.
    // Reading rows at width * 4 instead would shear the image.
    let width = 2;
    let pitch = 16; // four pixels' worth for a two-pixel row
    let red = argb(255, 255, 0, 0);
    let mut bytes = Vec::new();
    for _ in 0..2 {
        bytes.extend_from_slice(&red.to_ne_bytes());
        bytes.extend_from_slice(&red.to_ne_bytes());
        bytes.extend_from_slice(&[0xee; 8]); // padding, must never be read
    }

    let mut image = Image::new(2, 2);
    blit(
        &mut image,
        &Blit {
            bytes: &bytes,
            width,
            height: 2,
            pitch,
            format: SourceFormat::Argb8888,
            crtc_x: 0,
            crtc_y: 0,
            crtc_w: 0,
            crtc_h: 0,
        },
    );
    assert!(
        image.pixels().iter().all(|p| *p == red),
        "padding bytes leaked into the image: {:08x?}",
        image.pixels()
    );
}

#[test]
fn a_truncated_buffer_is_survived_rather_than_indexed_past() {
    // A framebuffer whose reported height and pitch exceed its mapping is a
    // driver bug; it must not be a panic.
    let bytes = solid(2, 1, argb(255, 255, 0, 0));
    let mut image = Image::new(4, 4);
    blit(
        &mut image,
        &Blit {
            bytes: &bytes,
            width: 2,
            height: 4, // claims four rows, has one
            pitch: 8,
            format: SourceFormat::Argb8888,
            crtc_x: 0,
            crtc_y: 0,
            crtc_w: 0,
            crtc_h: 0,
        },
    );
    // The first row is there; the rest was not readable and was skipped.
    assert_ne!(pixel_at(&image, 0, 0), 0);
}

#[test]
fn scaling_up_repeats_source_pixels() {
    // Nearest-neighbour: a 2x2 scaled to 4x4 is each pixel doubled. This is
    // where the reference resamples and this does not, which is the one
    // documented divergence in the capture path.
    let mut bytes = Vec::new();
    let colours = [
        argb(255, 255, 0, 0),
        argb(255, 0, 255, 0),
        argb(255, 0, 0, 255),
        argb(255, 255, 255, 255),
    ];
    for colour in colours {
        bytes.extend_from_slice(&colour.to_ne_bytes());
    }

    let mut image = Image::new(4, 4);
    blit(
        &mut image,
        &Blit {
            bytes: &bytes,
            width: 2,
            height: 2,
            pitch: 8,
            format: SourceFormat::Argb8888,
            crtc_x: 0,
            crtc_y: 0,
            crtc_w: 4,
            crtc_h: 4,
        },
    );

    assert_eq!(pixel_at(&image, 0, 0), colours[0]);
    assert_eq!(pixel_at(&image, 1, 1), colours[0], "top-left doubled");
    assert_eq!(pixel_at(&image, 2, 0), colours[1]);
    assert_eq!(pixel_at(&image, 0, 2), colours[2]);
    assert_eq!(pixel_at(&image, 3, 3), colours[3]);
}

#[test]
fn scaling_down_samples_without_reading_out_of_bounds() {
    let bytes = solid(8, 8, argb(255, 255, 0, 0));
    let mut image = Image::new(8, 8);
    blit(
        &mut image,
        &Blit {
            bytes: &bytes,
            width: 8,
            height: 8,
            pitch: 32,
            format: SourceFormat::Argb8888,
            crtc_x: 0,
            crtc_y: 0,
            crtc_w: 3,
            crtc_h: 3,
        },
    );
    assert_eq!(pixel_at(&image, 2, 2), argb(255, 255, 0, 0));
    assert_eq!(pixel_at(&image, 3, 3), 0, "nothing outside the 3x3");
}

#[test]
fn rgb565_widens_by_replication_not_by_shifting() {
    // Found on a Raspberry Pi 5: the console plane is RGB565, so a capture
    // that only reads the 32-bit formats has nothing to read at all. Widening
    // 5 and 6 bits to 8 by shifting alone leaves full-scale at 0xF8/0xFC,
    // which darkens every bright pixel by up to 3% -- invisible in isolation
    // and obvious across a gradient.
    let cases = [
        (0xffff_u16, argb(255, 255, 255, 255), "white stays white"),
        (0x0000, argb(255, 0, 0, 0), "black stays black"),
        (0xf800, argb(255, 255, 0, 0), "full red"),
        (0x07e0, argb(255, 0, 255, 0), "full green"),
        (0x001f, argb(255, 0, 0, 255), "full blue"),
    ];
    for (packed, want, what) in cases {
        let bytes = {
            let mut v = Vec::new();
            for _ in 0..4 {
                v.extend_from_slice(&packed.to_ne_bytes());
            }
            v
        };
        let mut image = Image::new(2, 2);
        blit(
            &mut image,
            &Blit {
                bytes: &bytes,
                width: 2,
                height: 2,
                pitch: 4, // two bytes per pixel
                format: SourceFormat::Rgb565,
                crtc_x: 0,
                crtc_y: 0,
                crtc_w: 0,
                crtc_h: 0,
            },
        );
        assert_eq!(pixel_at(&image, 0, 0), want, "{what}");
        assert_eq!(pixel_at(&image, 1, 1), want, "{what}, second row");
    }
}

#[test]
fn rgb565_is_always_opaque() {
    // There is no alpha channel to misread, and a 565 plane that composited as
    // transparent would simply not appear.
    let bytes = vec![0u8; 8];
    let mut image = Image::new(2, 2);
    blit(
        &mut image,
        &Blit {
            bytes: &bytes,
            width: 2,
            height: 2,
            pitch: 4,
            format: SourceFormat::Rgb565,
            crtc_x: 0,
            crtc_y: 0,
            crtc_w: 0,
            crtc_h: 0,
        },
    );
    assert!(
        image.pixels().iter().all(|p| *p == argb(255, 0, 0, 0)),
        "all-zero 565 is opaque black, not transparent"
    );
}
