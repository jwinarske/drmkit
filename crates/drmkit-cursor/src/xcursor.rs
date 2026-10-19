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
//! attacker-controlled. `xcursor` 0.3.11 sizes its pixel buffer from the
//! declared dimensions before reading: `take_bytes` does `vec![0; len]` and
//! *then* `read_exact`. Dimensions are bounded at `0x7fff` each, as libxcursor
//! bounds them, so a file of a few dozen bytes can still reserve 4.3 GB.
//!
//! What that costs depends on whether the reservation fails, which is worth
//! stating precisely because the answer is not "it always crashes":
//!
//! | condition | outcome |
//! |---|---|
//! | 64-bit, default overcommit | reservation succeeds lazily, parse then fails cleanly |
//! | 64-bit, cgroup `MemoryMax` | survives -- the pages are never faulted in |
//! | `RLIMIT_AS` (`ulimit -v`) | allocation fails, and the process **aborts** |
//! | 32-bit target | 4.3 GB does not fit in a 4 GB address space, so as above |
//!
//! The first three rows were measured -- on a 60 GB x86-64 host and an 8 GB
//! aarch64 board, the third under `ulimit -v 1048576`. The fourth is reasoning
//! from the address space, not a measurement: there was no 32-bit sysroot to
//! hand. drmkit targets 32-bit ARM, so it is not hypothetical.
//!
//! An abort is not recoverable: Rust does not unwind on allocation failure, so
//! there is nothing for a caller to catch and no check placed after the parse
//! can help. The C this port follows has the same over-allocation -- see
//! `third_party/xcursor-mini`, which mallocs `width * height * 4` before
//! reading a pixel -- but it checks the result for `NULL`, so it degrades to a
//! load failure. Rust has no `NULL` to check.
//!
//! Even where nothing crashes, reserving 4.3 GB of address space to reject a
//! 60-byte file is not a good way to fail. Everything here is read from a
//! slice that is already in memory, and every length is checked against what
//! remains before anything is allocated. The limits are the caller's, so a
//! fuzz target can drive the same code with the same bounds the real loader
//! uses.
//!
//! Filed as <https://github.com/esposm03/xcursor-rs/issues/30>.

use crate::CursorError;

/// What the loader will accept from a file.
///
/// The reference applies the same two bounds after libxcursor has parsed, by
/// which point each image was already allocated at its declared size; a
/// failed malloc there degrades to a load failure. Here they are enforced
/// during the parse, before anything is allocated -- and, as there, only to
/// the size being loaded.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// Frames of the chosen size. Real animated cursors ship a few dozen per
    /// size, and a file holds several sizes: Adwaita's busy cursor is six
    /// sizes of sixty frames.
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
/// Bytes in the file header: magic, header length, version, entry count.
const FILE_HEADER_LEN: u32 = 16;
/// The most table-of-contents entries libxcursor will read.
const MAX_ENTRIES: u32 = 0x1_0000;
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

/// Parse the images of one size out of a cursor file: the nominal size
/// nearest `requested`, chosen from the table of contents before any image is
/// read, as libxcursor chooses it.
///
/// Images come back in file order, which is the order the animation plays in.
/// The other sizes are never read, so their frames count against nothing.
///
/// # Errors
///
/// - [`CursorError::Malformed`] for a file that is not a cursor, is
///   truncated, or whose offsets do not point where they claim.
/// - [`CursorError::TooLarge`] for a file that exceeds `limits`: too many
///   frames of the chosen size, or one of them too big.
/// - [`CursorError::NoImages`] for a file that lists no image at all.
pub(crate) fn parse(
    bytes: &[u8],
    limits: Limits,
    requested: u32,
) -> Result<Vec<RawImage>, CursorError> {
    // libxcursor takes the size as a signed `int` and refuses a negative one,
    // which is what the reference's cast makes of anything past `i32::MAX`.
    if i32::try_from(requested).is_err() {
        return Err(CursorError::NoImages);
    }

    let mut reader = Reader::new(bytes);
    reader.tag(*MAGIC)?;
    let header_len = reader.u32_le()?;
    // A header shorter than its own four fields would put the table of
    // contents inside them; libxcursor refuses it, and so does this.
    if header_len < FILE_HEADER_LEN {
        return Err(CursorError::Malformed);
    }
    let _version = reader.u32_le()?;
    let entry_count = reader.u32_le()?;
    if entry_count > MAX_ENTRIES {
        return Err(CursorError::Malformed);
    }

    // The count is file-declared, so cap it against what the file could
    // actually hold before it is used to size anything. Each entry is 12
    // bytes, so a file cannot honestly declare more than it has room for.
    let entry_count = entry_count as usize;
    let room = bytes.len().saturating_sub(header_len as usize) / TOC_ENTRY_LEN;
    if entry_count > room {
        return Err(CursorError::Malformed);
    }

    // An image entry's subtype is its nominal size, which is what the size is
    // chosen from. Bounded by the room check above, so this cannot be made to
    // allocate more than the file's own length.
    reader.seek(header_len as usize)?;
    let mut entries: Vec<(u32, u32)> = Vec::new();
    for _ in 0..entry_count {
        let chunk_type = reader.u32_le()?;
        let subtype = reader.u32_le()?;
        let position = reader.u32_le()?;
        if chunk_type == CHUNK_IMAGE {
            entries.push((subtype, position));
        }
    }

    let chosen = nearest_size(entries.iter().map(|(size, _)| *size), requested)
        .ok_or(CursorError::NoImages)?;
    let offsets: Vec<u32> = entries
        .iter()
        .filter(|(size, _)| *size == chosen)
        .map(|(_, position)| *position)
        .collect();
    if offsets.len() > limits.max_frames {
        return Err(CursorError::TooLarge);
    }

    let mut images = Vec::with_capacity(offsets.len());
    for offset in offsets {
        reader.seek(offset as usize)?;
        images.push(parse_image(&mut reader, chosen, limits)?);
    }
    Ok(images)
}

/// Parse one image chunk, the reader positioned at its start.
///
/// The chunk must be what its table-of-contents entry said it was -- an image
/// filed under `listed_size` -- which is the one check libxcursor makes of the
/// chunk header. Its length and version fields go unread, as there: a writer
/// that bumps either still produces a cursor libxcursor shows.
fn parse_image(
    reader: &mut Reader<'_>,
    listed_size: u32,
    limits: Limits,
) -> Result<RawImage, CursorError> {
    let _header_len = reader.u32_le()?;
    let chunk_type = reader.u32_le()?;
    let nominal_size = reader.u32_le()?;
    let _version = reader.u32_le()?;
    if chunk_type != CHUNK_IMAGE || nominal_size != listed_size {
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

/// The nominal size in `sizes` closest to `requested`, as libxcursor's
/// `_XcursorFindBestSize` picks it.
///
/// Cursor files are multi-resolution packs. On a tie the size listed first
/// wins. A nominal size of zero stands for "none chosen yet" in libxcursor's
/// loop, so any other size displaces it, and a file listing only zero has no
/// size to offer at all.
fn nearest_size(sizes: impl Iterator<Item = u32>, requested: u32) -> Option<u32> {
    let mut best: u32 = 0;
    for size in sizes {
        if best == 0 || size.abs_diff(requested) < best.abs_diff(requested) {
            best = size;
        }
    }
    (best != 0).then_some(best)
}
