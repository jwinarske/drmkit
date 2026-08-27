// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Decorations without a plane, into a framebuffer.
//!
//! The tier of last resort: no KMS plane is available, so the decorations are
//! composited into whatever buffer the caller already presents — an fbdev
//! mapping, typically. Every frame costs a blend over the damaged region plus
//! a channel conversion, which is why it is last.

use drmkit_scene::{CompositeRect, CompositeSrc, blend_into, clear_into, convert_row};

use crate::{DamageRect, intersect_rect};

/// One decoration to blend in.
#[derive(Debug, Clone, Copy)]
pub struct BlitItem<'a> {
    /// Its pixels.
    pub pixels: &'a [u8],
    /// Bytes between its rows.
    pub stride: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// What its pixels are.
    pub fourcc: u32,
    /// Destination x.
    pub x: i32,
    /// Destination y.
    pub y: i32,
}

/// Pick a `FourCC` from what fbdev reports about the framebuffer.
///
/// `None` for a depth this cannot describe. Refusing is the point: a caller
/// handed a guess would write 32-bit pixels into a 24-bit buffer, and the
/// result is not a wrong colour but a sheared image, because every row lands
/// at the wrong offset.
///
/// The red-versus-blue offset decides channel order, which is the one thing
/// fbdev reports that cannot be inferred: a buffer described as 32bpp is
/// equally likely to be `XRGB8888` or `XBGR8888`, and getting it backwards
/// swaps red and blue on every pixel.
#[must_use]
pub const fn fb_fourcc_for(
    bpp: u32,
    red_offset: u32,
    blue_offset: u32,
    transparency_length: u32,
) -> Option<u32> {
    match bpp {
        32 => {
            // A zero-length transparency field means the buffer has no alpha
            // channel, whatever the byte width suggests -- the fourth byte is
            // padding, and describing it as alpha would have the display
            // engine blend against uninitialised data.
            let has_alpha = transparency_length != 0;
            Some(if red_offset > blue_offset {
                if has_alpha {
                    drmkit_fmt::fourcc::ARGB8888
                } else {
                    drmkit_fmt::fourcc::XRGB8888
                }
            } else if has_alpha {
                drmkit_fmt::fourcc::ABGR8888
            } else {
                drmkit_fmt::fourcc::XBGR8888
            })
        }
        16 => Some(if red_offset > blue_offset {
            drmkit_fmt::fourcc::RGB565
        } else {
            drmkit_fmt::fourcc::BGR565
        }),
        _ => None,
    }
}

/// The framebuffer being composited into.
#[derive(Debug)]
pub struct FbTarget<'a> {
    /// The mapping.
    pub pixels: &'a mut [u8],
    /// Bytes between rows.
    pub stride: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// What its pixels are, from [`fb_fourcc_for`].
    pub fourcc: u32,
}

/// Blend `items` into `target` over `damage`.
///
/// `shadow` is an `ARGB8888` scratch buffer the size of the framebuffer.
/// Compositing happens there and is converted into the framebuffer's own
/// format afterwards, rather than blending directly: a blend needs a
/// consistent channel order and an alpha channel, and the destination has
/// neither in general.
///
/// **Only the damaged rows are converted.** The blend is already confined to
/// the damage; converting the whole buffer afterwards would make the damage
/// tracking pointless, since the conversion is the same cost per row as the
/// blend.
///
/// Does nothing when the damage is empty, or when the shadow is too small for
/// the framebuffer it is meant to shadow.
pub fn compose_into_framebuffer(
    target: &mut FbTarget<'_>,
    shadow: &mut [u8],
    items: &[BlitItem<'_>],
    damage: DamageRect,
) {
    let (fb_width, fb_height, fb_stride, fb_fourcc) =
        (target.width, target.height, target.stride, target.fourcc);
    let shadow_stride = fb_width * 4;
    if damage.is_empty()
        || shadow.len() < shadow_stride as usize * fb_height as usize
        || fb_width == 0
        || fb_height == 0
    {
        return;
    }

    let rect = |region: DamageRect| CompositeRect {
        x: region.x,
        y: region.y,
        w: region.w,
        h: region.h,
    };

    clear_into(shadow, shadow_stride, fb_width, fb_height, rect(damage));

    for item in items {
        // Each decoration contributes only where it overlaps the damage.
        // Blending its whole extent would repaint pixels the damage says are
        // already correct, at the cost the damage exists to avoid.
        let overlap = intersect_rect(item.x, item.y, item.width, item.height, damage);
        if overlap.is_empty() {
            continue;
        }
        let src = CompositeSrc {
            pixels: item.pixels,
            src_stride_bytes: item.stride,
            src_width: item.width,
            src_height: item.height,
            drm_fourcc: item.fourcc,
            // Fully opaque: a decoration's transparency is in its own pixels,
            // and a plane alpha on top would fade the whole thing including
            // the parts meant to be solid.
            plane_alpha: u16::MAX,
        };
        blend_into(
            shadow,
            shadow_stride,
            fb_width,
            fb_height,
            &src,
            CompositeRect {
                x: overlap.x - item.x,
                y: overlap.y - item.y,
                w: overlap.w,
                h: overlap.h,
            },
            rect(overlap),
        );
    }

    let first = damage.y.max(0).unsigned_abs();
    let last = (first + damage.h).min(fb_height);
    for row in first..last {
        let to = row as usize * fb_stride as usize;
        let from = row as usize * shadow_stride as usize;
        let (Some(src), Some(dst)) = (
            shadow.get(from..from + shadow_stride as usize),
            target.pixels.get_mut(to..to + fb_stride as usize),
        ) else {
            // A framebuffer shorter than its own stride and height describe is
            // the caller's error; stopping short beats writing past its end.
            break;
        };
        convert_row(dst, src, fb_width as usize, fb_fourcc);
    }
}
