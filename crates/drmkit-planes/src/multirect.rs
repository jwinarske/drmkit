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

/// How many planes a CRTC can stack, from its `capabilities` blob, or `None`
/// when the blob does not say.
///
/// The downstream SDE driver publishes `max_blendstages=<n>` and, where a
/// stage is held back for secure UI, `sec_ui_blendstage=<s>`. A plane's zpos
/// `z` goes to stage `z + 1` -- stage 0 is the mixer's base -- so ordinary
/// planes have the stages below whichever of the two comes first. The
/// SA8155P publishes 11 and 10, so it stacks nine planes, zpos 0 to 8, while
/// every plane advertises a zpos range of `[0, 10]`; a zpos of 9 or 10 is
/// refused even on a lone plane (P-46).
///
/// As the reference's `parse_blend_limit` reads it: a `sec_ui_blendstage` of
/// 0 holds nothing back, and `use_baselayer_for_stage=1` makes the base a
/// stage a plane can use.
#[must_use]
pub fn parse_blend_stage_limit(caps_text: &[u8]) -> Option<usize> {
    let max = key_value(caps_text, b"max_blendstages=").filter(|max| *max != 0)?;
    let top = key_value(caps_text, b"sec_ui_blendstage=")
        .filter(|stage| *stage > 0 && *stage < max)
        .unwrap_or(max);
    let base_usable = key_value(caps_text, b"use_baselayer_for_stage=").unwrap_or(0) != 0;
    let limit = if base_usable { top } else { top - 1 };
    usize::try_from(limit).ok().filter(|limit| *limit > 0)
}

/// The unsigned 32-bit number after `key` at the start of a line of
/// `caps_text`. One that does not fit is no number, as for the reference.
fn key_value(caps_text: &[u8], key: &[u8]) -> Option<u32> {
    let mut from = 0;
    while let Some(offset) = caps_text[from..]
        .windows(key.len())
        .position(|window| window == key)
    {
        let pos = from + offset;
        if pos == 0 || caps_text[pos - 1] == b'\n' {
            let value = &caps_text[pos + key.len()..];
            let digits = value.iter().take_while(|b| b.is_ascii_digit()).count();
            return std::str::from_utf8(&value[..digits]).ok()?.parse().ok();
        }
        from = pos + key.len();
    }
    None
}
