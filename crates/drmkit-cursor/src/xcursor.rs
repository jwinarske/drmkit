// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Reading `.xcursor` files.
//!
//! The format is a header, a table of contents, and a chunk per image. Each
//! image chunk states its own dimensions and is followed by that many pixels.
//!
//! # Why this is not the `xcursor` crate
//!
//! Because a cursor file is untrusted input -- it comes from a user theme
//! directory or wherever `XCURSOR_PATH` points -- and every number in it is
//! attacker-controlled. `xcursor` 0.3.11 allocates from those numbers before
//! it has the bytes to back them: `take_bytes` does `vec![0; len]` and *then*
//! reads. A 60-byte file declaring a 32767x32767 image asks for 4.3 GB, and
//! since Rust aborts on allocation failure rather than unwinding, the process
//! dies -- there is nothing to catch and no guard placed after the parse can
//! help.
//!
//! Everything here is read from a slice that is already in memory, and every
//! length is checked against what remains before anything is allocated. The
//! limits are the caller's, so a fuzz target can drive the same code with the
//! same bounds the real loader uses.

use crate::CursorError;

/// What the loader will accept from a file.
///
/// The reference applies the same two bounds after parsing, which works there
/// because libxcursor validates against the real file size first. Here they
/// have to be enforced during the parse or they are enforced too late.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// Frames in one cursor. Real animated cursors ship a few dozen.
    pub max_frames: usize,
    /// Width or height of one frame. Larger than any real cursor, and far
    /// larger than any cursor plane.
    pub max_dim: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frames: 256,
            max_dim: 512,
        }
    }
}

/// One image out of a cursor file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawImage {
    /// The size the file nominally files this image under, which need not
    /// match its actual dimensions.
    pub nominal_size: u32,
    pub width: u32,
    pub height: u32,
    pub xhot: u32,
    pub yhot: u32,
    pub delay_ms: u32,
    /// Premultiplied `0xAARRGGBB`, row-major.
    pub pixels: Vec<u32>,
}

/// Magic at the start of every cursor file.
const MAGIC: &[u8; 4] = b"Xcur";
/// The table-of-contents type marking an image chunk.
const CHUNK_IMAGE: u32 = 0xfffd_0002;
/// Bytes in an image chunk's own header.
const IMAGE_HEADER_LEN: u32 = 0x24;
/// The only image chunk version there is.
const IMAGE_VERSION: u32 = 1;
/// Bytes per table-of-contents entry.
const TOC_ENTRY_LEN: usize = 12;

/// A bounds-checked reader over a byte slice.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn seek(&mut self, to: usize) -> Result<(), CursorError> {
        if to > self.bytes.len() {
            return Err(CursorError::Malformed);
        }
        self.at = to;
        Ok(())
    }

    fn u32_le(&mut self) -> Result<u32, CursorError> {
        let end = self.at.checked_add(4).ok_or(CursorError::Malformed)?;
        let slice = self.bytes.get(self.at..end).ok_or(CursorError::Malformed)?;
        self.at = end;
        Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    fn tag(&mut self, expected: [u8; 4]) -> Result<(), CursorError> {
        let end = self.at.checked_add(4).ok_or(CursorError::Malformed)?;
        let slice = self.bytes.get(self.at..end).ok_or(CursorError::Malformed)?;
        self.at = end;
        if slice == expected {
            Ok(())
        } else {
            Err(CursorError::Malformed)
        }
    }

    /// The bytes for `count` pixels, or an error if they are not all present.
    ///
    /// This is the check the whole module exists for: the length comes from
    /// the file, so it is verified against what the slice actually holds
    /// *before* any allocation is sized from it.
    fn pixel_bytes(&mut self, count: usize) -> Result<&'a [u8], CursorError> {
        let len = count.checked_mul(4).ok_or(CursorError::Malformed)?;
        let end = self.at.checked_add(len).ok_or(CursorError::Malformed)?;
        let slice = self.bytes.get(self.at..end).ok_or(CursorError::Malformed)?;
        self.at = end;
        Ok(slice)
    }
}

/// Parse every image in a cursor file.
///
/// Images come back in file order, which is the order the animation plays in.
///
/// # Errors
///
/// - [`CursorError::Malformed`] for a file that is not a cursor, is
///   truncated, or whose offsets do not point where they claim.
/// - [`CursorError::TooLarge`] for a file within `limits`' reach but past it:
///   too many frames, or a frame too big.
pub(crate) fn parse(bytes: &[u8], limits: Limits) -> Result<Vec<RawImage>, CursorError> {
    let mut reader = Reader::new(bytes);
    reader.tag(*MAGIC)?;
    let header_len = reader.u32_le()?;
    let _version = reader.u32_le()?;
    let entry_count = reader.u32_le()?;

    // The count is file-declared, so cap it against what the file could
    // actually hold before it is used to size anything. Each entry is 12
    // bytes, so a file cannot honestly declare more than it has room for.
    let entry_count = entry_count as usize;
    let room = bytes.len().saturating_sub(header_len as usize) / TOC_ENTRY_LEN;
    if entry_count > room {
        return Err(CursorError::Malformed);
    }

    reader.seek(header_len as usize)?;
    let mut image_offsets: Vec<u32> = Vec::new();
    for _ in 0..entry_count {
        let chunk_type = reader.u32_le()?;
        let _subtype = reader.u32_le()?;
        let position = reader.u32_le()?;
        if chunk_type == CHUNK_IMAGE {
            if image_offsets.len() >= limits.max_frames {
                return Err(CursorError::TooLarge);
            }
            image_offsets.push(position);
        }
    }

    let mut images = Vec::with_capacity(image_offsets.len());
    for offset in image_offsets {
        reader.seek(offset as usize)?;
        images.push(parse_image(&mut reader, limits)?);
    }
    Ok(images)
}

/// Parse one image chunk, the reader positioned at its start.
fn parse_image(reader: &mut Reader<'_>, limits: Limits) -> Result<RawImage, CursorError> {
    if reader.u32_le()? != IMAGE_HEADER_LEN {
        return Err(CursorError::Malformed);
    }
    if reader.u32_le()? != CHUNK_IMAGE {
        return Err(CursorError::Malformed);
    }
    let nominal_size = reader.u32_le()?;
    if reader.u32_le()? != IMAGE_VERSION {
        return Err(CursorError::Malformed);
    }
    let width = reader.u32_le()?;
    let height = reader.u32_le()?;
    let xhot = reader.u32_le()?;
    let yhot = reader.u32_le()?;
    let delay_ms = reader.u32_le()?;

    if width == 0 || height == 0 {
        return Err(CursorError::Malformed);
    }
    // Before the pixel count is computed from them, let alone allocated from.
    if width > limits.max_dim || height > limits.max_dim {
        return Err(CursorError::TooLarge);
    }
    // libxcursor rejects a hotspot outside the image; so does the format.
    if xhot > width || yhot > height {
        return Err(CursorError::Malformed);
    }

    // Both bounded above, so this cannot overflow.
    let count = width as usize * height as usize;
    let raw = reader.pixel_bytes(count)?;

    // The format stores each pixel as a little-endian 32-bit ARGB value, so a
    // plain little-endian read is already 0xAARRGGBB -- the same convention
    // the rest of the tree uses for a premultiplied pixel.
    let pixels = raw
        .chunks_exact(4)
        .map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]]))
        .collect();

    Ok(RawImage {
        nominal_size,
        width,
        height,
        xhot,
        yhot,
        delay_ms,
        pixels,
    })
}

/// The nominal size in `images` closest to `requested`.
///
/// Cursor files are multi-resolution packs. Ties go to the smaller size: a
/// cursor slightly too small sits inside the plane, where one slightly too
/// large has to be cropped or scaled.
pub(crate) fn nearest_size(images: &[RawImage], requested: u32) -> Option<u32> {
    images
        .iter()
        .map(|image| image.nominal_size)
        .min_by_key(|size| (size.abs_diff(requested), *size))
}
