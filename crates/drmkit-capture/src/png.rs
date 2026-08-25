// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Write an [`Image`] out as a PNG.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use crate::CaptureError;
use crate::image::{Image, unpremultiply};

/// Encode `image` as an 8-bit RGBA PNG at `path`, creating or truncating it.
///
/// Alpha is written straight, not premultiplied, because that is what PNG
/// means by an alpha channel and what every tool that opens the file will
/// assume. An opaque screenshot round-trips exactly; a translucent pixel comes
/// back to within the precision the premultiply left it, which is not much at
/// low alpha and is not recoverable by anyone.
///
/// The directory must already exist.
///
/// # Errors
///
/// - [`CaptureError::EmptyImage`] if `image` has no pixels. Writing a
///   zero-by-zero PNG would produce a file no decoder accepts, so it fails
///   here rather than at whoever opens it.
/// - [`CaptureError::Io`] if the file cannot be created or written.
pub fn write_png(image: &Image, path: impl AsRef<Path>) -> Result<(), CaptureError> {
    if image.is_empty() {
        return Err(CaptureError::EmptyImage);
    }

    let mut rgba = Vec::with_capacity(image.pixels().len() * 4);
    for pixel in image.pixels() {
        rgba.extend_from_slice(&unpremultiply(*pixel));
    }

    let file = File::create(path).map_err(CaptureError::Io)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), image.width(), image.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    let mut writer = encoder.write_header().map_err(png_io)?;
    writer.write_image_data(&rgba).map_err(png_io)?;
    writer.finish().map_err(png_io)
}

/// Reduce a `png::EncodingError` to the underlying I/O failure.
///
/// Everything the encoder can complain about here that is *not* I/O -- a
/// header that disagrees with the data, an unsupported colour type -- would be
/// this module miscalling it rather than anything the caller can act on, so
/// those collapse into a generic write failure rather than a variant nobody
/// can handle differently.
fn png_io(error: png::EncodingError) -> CaptureError {
    match error {
        png::EncodingError::IoError(io) => CaptureError::Io(io),
        other => CaptureError::Io(std::io::Error::other(other.to_string())),
    }
}
