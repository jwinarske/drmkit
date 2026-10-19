// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Writing a capture as a baseline JPEG.
//!
//! Port of `src/capture/jpg.{hpp,cpp}`. Behind the `jpeg` feature, as upstream
//! puts it behind a build switch and for the same reason: a second image
//! format is not something every consumer wants, and the dependency should
//! follow the want.
//!
//! JPEG is lossy and has no alpha. Both matter to a caller choosing between
//! this and PNG: a screenshot of a translucent decoration keeps its
//! translucency in PNG and is flattened here, and text will show ringing.
//! What it buys is size, which is why a camera frame goes out this way.

use std::io::BufWriter;
use std::path::Path;

use crate::{CaptureError, Image, unpremultiply};

/// The quality range a JPEG encoder accepts.
const QUALITY_RANGE: std::ops::RangeInclusive<u8> = 1..=100;

/// The largest width or height libjpeg writes (`JPEG_MAX_DIMENSION`). The
/// format's own field is 16 bits; the reference refuses past this, as libjpeg
/// does, rather than writing a frame the field cannot describe.
const MAX_DIMENSION: u32 = 65_500;

/// Both dimensions as the encoder takes them, or the reference's refusal.
fn dimensions(width: u32, height: u32) -> Result<(u16, u16), CaptureError> {
    let fits = |value: u32| u16::try_from(value).ok().filter(|_| value <= MAX_DIMENSION);
    fits(width).zip(fits(height)).ok_or_else(|| {
        CaptureError::Io(std::io::Error::other(format!(
            "{width}x{height} is past JPEG's {MAX_DIMENSION} pixel limit"
        )))
    })
}

/// An encoder writing 4:2:0, as the reference's libjpeg does at every quality.
///
/// `jpeg-encoder` keeps chroma at full resolution from quality 90 up, and 90
/// is the quality the reference's callers ask for, so left to its default the
/// common case would write a different and larger file than the reference.
fn encoder(file: std::fs::File, quality: u8) -> jpeg_encoder::Encoder<BufWriter<std::fs::File>> {
    let quality = quality.clamp(*QUALITY_RANGE.start(), *QUALITY_RANGE.end());
    let mut encoder = jpeg_encoder::Encoder::new(BufWriter::new(file), quality);
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
    encoder
}

/// Write `image` as a baseline JPEG.
///
/// `quality` is clamped into `1..=100` rather than refused. It is a knob a
/// caller passes through from a config file or a command line, and a
/// screenshot is not worth failing over a number that only ever means
/// "as good as possible" or "as small as possible" at the ends.
///
/// The clamp is belt-and-braces: the encoder accepts `0` and `255` without
/// complaint, so removing it changes no observable behavior — verified by
/// injection. It stays because that is the encoder's business rather than a
/// promise, and a caller reading this signature should be able to trust the
/// stated range without checking what the encoder does with the rest.
///
/// The pixels are un-premultiplied first, exactly as the PNG path does.
/// Encoding premultiplied color without its alpha darkens every partly
/// transparent pixel toward black — a translucent decoration would come out
/// looking like a dark one, rather than in its own color at full opacity.
///
/// # Errors
///
/// [`CaptureError::EmptyImage`] for an image with no pixels, and
/// [`CaptureError::Io`] if a dimension is past 65,500 pixels or the file
/// cannot be written.
pub fn write_jpg(image: &Image, path: impl AsRef<Path>, quality: u8) -> Result<(), CaptureError> {
    if image.is_empty() {
        return Err(CaptureError::EmptyImage);
    }
    let (width, height) = dimensions(image.width(), image.height())?;

    let ycbcr = averaged_ycbcr(image);

    let file = std::fs::File::create(path).map_err(CaptureError::Io)?;
    encoder(file, quality)
        .encode(&ycbcr, width, height, jpeg_encoder::ColorType::Ycbcr)
        .map_err(|error| CaptureError::Io(std::io::Error::other(error)))
}

/// libjpeg's fixed-point scale for color conversion (`SCALEBITS`).
const SCALE_BITS: u32 = 16;
/// One half at that scale, for rounding (`ONE_HALF`).
const ONE_HALF: i32 = 1 << (SCALE_BITS - 1);
/// The chroma offset, 128 at that scale (`CBCR_OFFSET`).
const CBCR_OFFSET: i32 = 128 << SCALE_BITS;

/// A coefficient at libjpeg's fixed-point scale (`FIX`).
#[expect(
    clippy::cast_possible_truncation,
    reason = "every coefficient is below 1.0, so the scaled value fits"
)]
const fn fix(value: f64) -> i32 {
    (value * (1 << SCALE_BITS) as f64 + 0.5) as i32
}

/// One pixel to `YCbCr`, exactly as libjpeg's `rgb_ycc_convert` computes it.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the coefficients of each row sum to at most one, so every \
              result is in 0..=255 by construction, as in libjpeg"
)]
fn ycbcr(r: u8, g: u8, b: u8) -> [u8; 3] {
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    let y = (fix(0.299) * r + fix(0.587) * g + fix(0.114) * b + ONE_HALF) >> SCALE_BITS;
    let cb = (-fix(0.168_74) * r - fix(0.331_26) * g + fix(0.5) * b + CBCR_OFFSET + ONE_HALF - 1)
        >> SCALE_BITS;
    let cr = (fix(0.5) * r - fix(0.418_69) * g - fix(0.081_31) * b + CBCR_OFFSET + ONE_HALF - 1)
        >> SCALE_BITS;
    [y as u8, cb as u8, cr as u8]
}

/// The image as interleaved `YCbCr`, each chroma sample the average of the
/// two-by-two block it covers, as libjpeg's 4:2:0 downsampling makes it.
///
/// The encoder subsamples chroma by taking one pixel of each block, which
/// throws away three samples in four: one-pixel detail in color comes out in
/// the wrong hue. Averaging here first, and repeating the average across the
/// block, leaves the encoder nothing to lose. libjpeg's rounding is kept too:
/// a bias that alternates 1, 2 across each row of blocks, and a block at an
/// odd right or bottom edge padded by repeating its last column or row.
///
/// JPEG has no alpha, so the pixels are un-premultiplied and the fourth
/// channel dropped here rather than encoded and ignored.
fn averaged_ycbcr(image: &Image) -> Vec<u8> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let full: Vec<[u8; 3]> = image
        .pixels()
        .iter()
        .map(|pixel| {
            let [r, g, b, _] = unpremultiply(*pixel);
            ycbcr(r, g, b)
        })
        .collect();

    let mut out = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        let top = y & !1;
        let bottom = (top + 1).min(height - 1);
        for x in 0..width {
            let left = x & !1;
            let right = (left + 1).min(width - 1);
            let bias = 1 + u32::from((x / 2) % 2 == 1);
            let average = |channel: usize| {
                let sum = [(top, left), (top, right), (bottom, left), (bottom, right)]
                    .iter()
                    .map(|&(row, col)| u32::from(full[row * width + col][channel]))
                    .sum::<u32>();
                u8::try_from((sum + bias) >> 2).unwrap_or(u8::MAX)
            };
            out.extend_from_slice(&[full[y * width + x][0], average(1), average(2)]);
        }
    }
    out
}

/// One `NV12` frame, as a camera or decoder hands it over.
///
/// Strides rather than packed rows because a capture buffer is padded to
/// whatever the hardware wanted, and copying it tight first would be a second
/// pass over every byte for nothing.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers, with no default to fill a new field from"
)]
pub struct Nv12Frame<'a> {
    /// `height` rows of `luma_stride`.
    pub luma: &'a [u8],
    /// Bytes between luma rows.
    pub luma_stride: usize,
    /// `height.div_ceil(2)` rows of interleaved Cb/Cr, each at least
    /// `width.div_ceil(2) * 2` bytes, at `chroma_stride`.
    pub chroma: &'a [u8],
    /// Bytes between chroma rows.
    pub chroma_stride: usize,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Write an `NV12` frame as a baseline JPEG, without converting to RGB first.
///
/// The path a camera or a decoder takes. `NV12` is already YCbCr 4:2:0, which
/// is what JPEG stores — so converting to RGB and back would lose a little
/// color to two rounding trips and cost both conversions, for nothing.
///
/// # Errors
///
/// [`CaptureError::EmptyImage`] for zero dimensions, strides narrower than a
/// row, or planes too short for the dimensions given, and [`CaptureError::Io`]
/// if a dimension is past 65,500 pixels or the file cannot be written.
pub fn write_jpg_nv12(
    frame: &Nv12Frame<'_>,
    path: impl AsRef<Path>,
    quality: u8,
) -> Result<(), CaptureError> {
    let Nv12Frame {
        luma,
        luma_stride,
        chroma,
        chroma_stride,
        width,
        height,
    } = *frame;
    if width == 0 || height == 0 {
        return Err(CaptureError::EmptyImage);
    }
    let (encoded_width, encoded_height) = dimensions(width, height)?;
    let (w, h) = (width as usize, height as usize);
    // Chroma is subsampled by two in both directions, and an odd dimension
    // rounds *up* -- a 33-pixel-wide frame has 17 chroma columns, not 16, and
    // sizing for 16 reads past the last row.
    let chroma_rows = h.div_ceil(2);
    if luma_stride < w
        || chroma_stride < w.div_ceil(2) * 2
        || luma.len() < luma_stride * h
        || chroma.len() < chroma_stride * chroma_rows
    {
        return Err(CaptureError::EmptyImage);
    }

    // Repack to packed rows: the encoder wants no padding, and a camera
    // buffer has whatever the hardware chose.
    let mut packed_luma = Vec::with_capacity(w * h);
    for row in 0..h {
        let start = row * luma_stride;
        packed_luma.extend_from_slice(&luma[start..start + w]);
    }
    let chroma_cols = w.div_ceil(2) * 2;
    let mut packed_chroma = Vec::with_capacity(chroma_cols * chroma_rows);
    for row in 0..chroma_rows {
        let start = row * chroma_stride;
        packed_chroma.extend_from_slice(&chroma[start..start + chroma_cols]);
    }

    let file = std::fs::File::create(path).map_err(CaptureError::Io)?;
    encoder(file, quality)
        .encode(
            &interleave_nv12(&packed_luma, &packed_chroma, w, h),
            encoded_width,
            encoded_height,
            jpeg_encoder::ColorType::Ycbcr,
        )
        .map_err(|error| CaptureError::Io(std::io::Error::other(error)))
}

/// Expand semi-planar `NV12` into the interleaved `YCbCr` the encoder takes.
///
/// The chroma planes are half-resolution, so each chroma sample is repeated
/// across the two-by-two block of luma it covers. That is what 4:2:0 *means*;
/// the encoder subsamples it again on the way out, arriving back where it
/// started without a color-space round trip through RGB.
fn interleave_nv12(luma: &[u8], chroma: &[u8], width: usize, height: usize) -> Vec<u8> {
    let chroma_cols = width.div_ceil(2) * 2;
    let mut out = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            let cb_index = (y / 2) * chroma_cols + (x / 2) * 2;
            out.push(luma.get(y * width + x).copied().unwrap_or(0));
            out.push(chroma.get(cb_index).copied().unwrap_or(128));
            out.push(chroma.get(cb_index + 1).copied().unwrap_or(128));
        }
    }
    out
}
