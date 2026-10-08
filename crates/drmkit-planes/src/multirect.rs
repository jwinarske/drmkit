// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Multirect ("smart DMA") plane pairing.
//!
//! Port of `src/planes/multirect.hpp` (drm-cxx `8bf20e6`, `4b366b5`).
//!
//! Some display controllers drive two rectangles from one hardware source
//! pipe and publish the second rectangle as a separate, "virtual" DRM plane.
//! Such a plane is only valid while its parent -- the pipe's first rectangle
//! -- is in use on the same commit; staged alone, the kernel refuses it. The
//! downstream SDE display driver marks these planes in the read-only
//! `capabilities` blob with a `primary_smart_plane_id=<parent>` line. The
//! SA8155P has seven. Drivers without that key are unaffected by everything
//! here.

const KEY: &[u8] = b"primary_smart_plane_id=";

/// The parent plane id from a `capabilities` blob's text, or `None` when the
/// blob does not mark the plane as a multirect rectangle, or the value is
/// garbage.
///
/// Bytes rather than `&str`: the blob is driver-private and nothing promises
/// it is UTF-8, while the key and the number are ASCII either way.
#[must_use]
pub fn parse_multirect_parent(caps_text: &[u8]) -> Option<u32> {
    let mut from = 0;
    while let Some(offset) = caps_text[from..]
        .windows(KEY.len())
        .position(|window| window == KEY)
    {
        let pos = from + offset;
        // Only a match at the start of a line counts: the blob is key=value
        // lines.
        if pos == 0 || caps_text[pos - 1] == b'\n' {
            let value = &caps_text[pos + KEY.len()..];
            let digits = value.iter().take_while(|b| b.is_ascii_digit()).count();
            return std::str::from_utf8(&value[..digits])
                .ok()
                .and_then(|text| text.parse::<u32>().ok())
                .filter(|id| *id != 0);
        }
        from = pos + KEY.len();
    }
    None
}

/// Whether a plane whose multirect parent is `parent` may be placed, given
/// the planes already in use: a plain plane always, a virtual one only once
/// its parent is in use. `in_use(id)` reports whether a plane id is taken.
#[must_use]
pub fn multirect_pairing_ok(parent: Option<u32>, in_use: impl Fn(u32) -> bool) -> bool {
    parent.is_none_or(in_use)
}
