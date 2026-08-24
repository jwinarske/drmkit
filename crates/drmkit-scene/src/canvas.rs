// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Software composition for layers the allocator could not place.
//!
//! When a scene has more layers than the CRTC has usable planes, the ones that
//! did not get a plane still have to reach the screen. They are blended on the
//! CPU into a single full-screen ARGB8888 surface, in stacking order, and that
//! surface goes on a plane of its own.
//!
//! This module is the blend arithmetic, and nothing else. It reads and writes
//! plain byte slices, so every rule below — clipping, scaling, premultiplied
//! `SRC_OVER`, per-plane alpha — is exercised by ordinary unit tests with no
//! device anywhere. Upstream splits it the same way and for the same reason.
//!
//! # Premultiplied, not straight
//!
//! Both source and destination pixels are assumed **premultiplied**, matching
//! the kernel's default `pixel blend mode` and the output convention of
//! `Blend2D`, thorvg, Cairo and Skia:
//!
//! ```text
//! out = src + dst * (1 - src_alpha)
//! ```
//!
//! A straight-alpha source blended by this formula does not merely look
//! slightly off — the colour channels saturate instead of mixing. Convert
//! before calling.
//!
//! # Byte order
//!
//! `ARGB8888` is `B, G, R, A` in memory, so on a little-endian host a pixel
//! read as a native `u32` is `0xAARRGGBB`. The arithmetic here is written in
//! those terms, as upstream's is.

use drmkit_fmt::fourcc;

/// A rectangle in whole pixels, half-open in extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompositeRect {
    /// Left edge. May be negative; the region is clipped.
    pub x: i32,
    /// Top edge. May be negative; the region is clipped.
    pub y: i32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

/// A source surface to blend from.
#[derive(Debug, Clone, Copy)]
pub struct CompositeSrc<'a> {
    /// The pixels, tightly packed rows of `src_stride_bytes`.
    pub pixels: &'a [u8],
    /// Distance between row starts, which may exceed `src_width * 4`.
    pub src_stride_bytes: u32,
    /// Width in pixels.
    pub src_width: u32,
    /// Height in pixels.
    pub src_height: u32,
    /// The pixel format. Only `ARGB8888` and `XRGB8888` blend today.
    pub drm_fourcc: u32,
    /// The layer's plane alpha, on the KMS `u16` scale.
    pub plane_alpha: u16,
}

/// Whether this module can blend from `fourcc`.
///
/// Deliberately narrow. A format that reaches the blend loop unrecognised
/// would be reinterpreted as `ARGB8888` and produce colour noise, so an
/// unsupported source is refused rather than guessed at.
#[must_use]
pub const fn format_supported(drm_fourcc: u32) -> bool {
    matches!(drm_fourcc, fourcc::ARGB8888 | fourcc::XRGB8888)
}

/// Whether a format carries no usable alpha channel.
const fn source_is_opaque(drm_fourcc: u32) -> bool {
    drm_fourcc == fourcc::XRGB8888
}

/// One premultiplied `SRC_OVER` step.
///
/// `out = src + dst * (1 - src_alpha)`, per channel, in `u32` lanes with
/// round-to-nearest. The alpha channel accumulates coverage so a run of
/// partially transparent sources composites to the right combined alpha, and
/// saturates rather than wrapping.
fn blend_pixel_over(src: u32, dst: u32) -> u32 {
    let sa = (src >> 24) & 0xFF;
    // Both shortcuts are exact, not approximations: a fully transparent source
    // contributes nothing, and a fully opaque one replaces the destination.
    if sa == 0 {
        return dst;
    }
    if sa == 0xFF {
        return src;
    }

    let inv = 255 - sa;
    let scale = |c: u32| (c * inv + 127) / 255;

    let out_r = ((src >> 16) & 0xFF) + scale((dst >> 16) & 0xFF);
    let out_g = ((src >> 8) & 0xFF) + scale((dst >> 8) & 0xFF);
    let out_b = (src & 0xFF) + scale(dst & 0xFF);
    let out_a = sa + scale((dst >> 24) & 0xFF);

    let clamp = |c: u32| if c > 0xFF { 0xFF } else { c };
    (clamp(out_a) << 24) | (clamp(out_r) << 16) | (clamp(out_g) << 8) | clamp(out_b)
}

/// Scale every channel of a premultiplied pixel by `alpha8 / 255`.
///
/// Applied whole-surface, from the layer's plane alpha. Scaling the colour
/// channels as well as alpha is what keeps the pixel premultiplied — leaving
/// `rgb > a` would break the assumption [`blend_pixel_over`] rests on and show
/// up as haloing at the edges.
fn modulate_pixel(pixel: u32, alpha8: u32) -> u32 {
    let scale = |c: u32| (c * alpha8 + 127) / 255;
    (scale((pixel >> 24) & 0xFF) << 24)
        | (scale((pixel >> 16) & 0xFF) << 16)
        | (scale((pixel >> 8) & 0xFF) << 8)
        | scale(pixel & 0xFF)
}

/// KMS plane alpha is `u16`; the blend works in `0..=255`.
///
/// Round-to-nearest, which matters in the middle of the range rather than at
/// the ends: `0xFFFF` and `0` land on `0xFF` and `0` whether rounded or
/// truncated, but a half-transparent `0x8000` truncates to `0x7F` and rounds
/// to `0x80`. One step darker than the reference, on every composited pixel of
/// every partially transparent layer.
const fn plane_alpha8(plane_alpha: u16) -> u32 {
    (plane_alpha as u32 * 255 + 32767) / 65535
}

/// The visible span of one axis after clipping both rectangles.
#[derive(Debug, Clone, Copy)]
struct AxisRange {
    dst_start: u32,
    dst_end: u32,
    src_start: u32,
    src_span: u32,
}

/// Clip one axis of a blit against both buffers.
///
/// Returns `None` when nothing survives — off-screen either side, or a
/// degenerate extent. Negative positions clip rather than wrap.
fn clip_axis(
    dst_pos: i32,
    dst_size: u32,
    dst_extent: u32,
    src_pos: i32,
    src_size: u32,
    src_extent: u32,
) -> Option<AxisRange> {
    if dst_size == 0 || src_size == 0 {
        return None;
    }

    // Each side is clipped against its own buffer independently; the mapping
    // between them is re-derived afterwards from what survived.
    let clip = |pos: i32, size: u32, extent: u32| -> Option<(u32, u32)> {
        let clipped_off = if pos < 0 { pos.unsigned_abs() } else { 0 };
        let start = pos.saturating_add(clipped_off.cast_signed());
        if start >= extent.cast_signed() {
            return None;
        }
        let start = start.cast_unsigned();
        let remaining = size.checked_sub(clipped_off)?;
        if remaining == 0 {
            return None;
        }
        let visible = remaining.min(extent - start);
        if visible == 0 {
            None
        } else {
            Some((start, visible))
        }
    };

    let (src_start, src_span) = clip(src_pos, src_size, src_extent)?;
    let (dst_start, dst_visible) = clip(dst_pos, dst_size, dst_extent)?;

    Some(AxisRange {
        dst_start,
        dst_end: dst_start + dst_visible,
        src_start,
        src_span,
    })
}

/// Map a destination index back to a source index, nearest-neighbour.
///
/// The same expression covers magnification and minification, so a scaled
/// blit needs no separate path.
const fn map_src_index(dst_idx: u32, dst_visible: u32, src_span: u32) -> u32 {
    if dst_visible <= 1 {
        return 0;
    }
    (dst_idx * src_span) / dst_visible
}

/// Whether a buffer is large enough for `height` rows of `stride` bytes.
///
/// Computed in `u64` so a stride and height that overflow `u32` are rejected
/// rather than wrapping into a small figure that passes the check.
fn fits(len: usize, height: u32, stride: u32) -> bool {
    let required = u64::from(height) * u64::from(stride);
    required <= len as u64
}

/// Zero-fill a rectangle of `dst`, clipped to its extents.
///
/// Zero is transparent black in premultiplied ARGB, which is the identity for
/// `SRC_OVER` — so this is what makes a frame's blending independent of the
/// last frame's.
pub fn clear_into(
    dst: &mut [u8],
    dst_stride_bytes: u32,
    dst_width: u32,
    dst_height: u32,
    rect: CompositeRect,
) {
    if dst_width == 0 || dst_height == 0 {
        return;
    }
    if u64::from(dst_stride_bytes) < u64::from(dst_width) * 4
        || !fits(dst.len(), dst_height, dst_stride_bytes)
    {
        return;
    }
    let Some(xr) = clip_axis(rect.x, rect.w, dst_width, 0, rect.w, dst_width) else {
        return;
    };
    let Some(yr) = clip_axis(rect.y, rect.h, dst_height, 0, rect.h, dst_height) else {
        return;
    };

    for y in yr.dst_start..yr.dst_end {
        let row = (y * dst_stride_bytes) as usize;
        let from = row + (xr.dst_start * 4) as usize;
        let to = row + (xr.dst_end * 4) as usize;
        dst[from..to].fill(0);
    }
}

/// Blend `src` over `dst`, premultiplied `SRC_OVER`.
///
/// Both rectangles are clipped against their own buffer, and mismatched sizes
/// scale nearest-neighbour. Refuses rather than guessing when the source
/// format is unsupported or either buffer is smaller than its stated geometry.
///
/// Unlike the C++, this needs no alignment guard. Upstream reinterpret-casts
/// both buffers to `uint32_t*` and has to bail on an unaligned base to avoid
/// undefined behaviour; reading through `chunks_exact` has no such
/// requirement, so a caller cannot hand in a pointer that makes this unsound.
pub fn blend_into(
    dst: &mut [u8],
    dst_stride_bytes: u32,
    dst_width: u32,
    dst_height: u32,
    src: &CompositeSrc<'_>,
    src_rect: CompositeRect,
    dst_rect: CompositeRect,
) {
    if dst_width == 0 || dst_height == 0 || !format_supported(src.drm_fourcc) {
        return;
    }
    if u64::from(dst_stride_bytes) < u64::from(dst_width) * 4
        || !fits(dst.len(), dst_height, dst_stride_bytes)
    {
        return;
    }
    if u64::from(src.src_stride_bytes) < u64::from(src.src_width) * 4
        || !fits(src.pixels.len(), src.src_height, src.src_stride_bytes)
    {
        return;
    }

    let Some(xr) = clip_axis(
        dst_rect.x,
        dst_rect.w,
        dst_width,
        src_rect.x,
        src_rect.w,
        src.src_width,
    ) else {
        return;
    };
    let Some(yr) = clip_axis(
        dst_rect.y,
        dst_rect.h,
        dst_height,
        src_rect.y,
        src_rect.h,
        src.src_height,
    ) else {
        return;
    };

    let opaque = source_is_opaque(src.drm_fourcc);
    let alpha8 = plane_alpha8(src.plane_alpha);
    let modulate = alpha8 < 0xFF;
    let visible_x = xr.dst_end - xr.dst_start;
    let visible_y = yr.dst_end - yr.dst_start;

    for dy in 0..visible_y {
        let sy = yr.src_start + map_src_index(dy, visible_y, yr.src_span);
        let src_row = (sy * src.src_stride_bytes) as usize;
        let dst_row = ((yr.dst_start + dy) * dst_stride_bytes) as usize;

        for dx in 0..visible_x {
            let sx = xr.src_start + map_src_index(dx, visible_x, xr.src_span);
            let s_at = src_row + (sx * 4) as usize;
            let d_at = dst_row + ((xr.dst_start + dx) * 4) as usize;

            let mut pixel = u32::from_ne_bytes([
                src.pixels[s_at],
                src.pixels[s_at + 1],
                src.pixels[s_at + 2],
                src.pixels[s_at + 3],
            ]);
            // An XRGB source's fourth byte is undefined padding, not alpha.
            // Reading it as alpha would make an opaque layer randomly
            // transparent wherever the producer left the byte clear.
            if opaque {
                pixel |= 0xFF00_0000;
            }
            if modulate {
                pixel = modulate_pixel(pixel, alpha8);
            }

            let under =
                u32::from_ne_bytes([dst[d_at], dst[d_at + 1], dst[d_at + 2], dst[d_at + 3]]);
            let out = blend_pixel_over(pixel, under).to_ne_bytes();
            dst[d_at..d_at + 4].copy_from_slice(&out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canvas of `w * h` pixels, tightly strided.
    fn canvas(w: u32, h: u32) -> Vec<u8> {
        vec![0; (w * h * 4) as usize]
    }

    fn px(buf: &[u8], stride: u32, x: u32, y: u32) -> u32 {
        let at = (y * stride + x * 4) as usize;
        u32::from_ne_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
    }

    fn solid(w: u32, h: u32, argb: u32) -> Vec<u8> {
        argb.to_ne_bytes().repeat((w * h) as usize)
    }

    fn src(pixels: &[u8], w: u32, h: u32, fourcc: u32) -> CompositeSrc<'_> {
        CompositeSrc {
            pixels,
            src_stride_bytes: w * 4,
            src_width: w,
            src_height: h,
            drm_fourcc: fourcc,
            plane_alpha: 0xFFFF,
        }
    }

    fn whole(w: u32, h: u32) -> CompositeRect {
        CompositeRect { x: 0, y: 0, w, h }
    }

    #[test]
    fn an_opaque_source_replaces_what_was_under_it() {
        let mut dst = solid(4, 4, 0xFF00_00FF);
        let pixels = solid(4, 4, 0xFFFF_0000);
        blend_into(
            &mut dst,
            16,
            4,
            4,
            &src(&pixels, 4, 4, fourcc::ARGB8888),
            whole(4, 4),
            whole(4, 4),
        );
        assert_eq!(px(&dst, 16, 2, 2), 0xFFFF_0000);
    }

    #[test]
    fn a_fully_transparent_source_leaves_the_destination_alone() {
        let mut dst = solid(4, 4, 0xFF00_00FF);
        let pixels = solid(4, 4, 0x0000_0000);
        blend_into(
            &mut dst,
            16,
            4,
            4,
            &src(&pixels, 4, 4, fourcc::ARGB8888),
            whole(4, 4),
            whole(4, 4),
        );
        assert_eq!(px(&dst, 16, 1, 1), 0xFF00_00FF);
    }

    /// Half-alpha red, premultiplied, over opaque blue.
    ///
    /// Premultiplied means the source's own channels are *already* scaled by
    /// its alpha, so they are added rather than interpolated:
    /// `out = src + dst * (1 - src_a)`. Reading 0x80 red as "half of 0xFF red"
    /// and lerping would give a different, wrong answer — which is exactly the
    /// mistake a straight-alpha source causes.
    #[test]
    fn a_half_alpha_source_composites_premultiplied() {
        let mut dst = solid(2, 2, 0xFF00_00FF);
        let pixels = solid(2, 2, 0x8080_0000);
        blend_into(
            &mut dst,
            8,
            2,
            2,
            &src(&pixels, 2, 2, fourcc::ARGB8888),
            whole(2, 2),
            whole(2, 2),
        );

        // inv = 0x7F. blue: 0x00 + (0xFF * 0x7F + 127)/255 = 0x7F.
        // red:  0x80 + (0x00 * ...)                        = 0x80.
        // a:    0x80 + (0xFF * 0x7F + 127)/255             = 0xFF.
        assert_eq!(px(&dst, 8, 0, 0), 0xFF80_007F);
    }

    /// An XRGB source is opaque even where its fourth byte is clear.
    ///
    /// That byte is undefined padding, not alpha. Trusting it would make an
    /// opaque layer randomly transparent wherever the producer happened to
    /// leave it zero — and a producer that memsets its buffer leaves all of it
    /// zero, so the layer would vanish entirely.
    #[test]
    fn an_xrgb_source_ignores_its_padding_byte() {
        let mut dst = solid(2, 2, 0xFF00_00FF);
        let pixels = solid(2, 2, 0x00FF_0000);
        blend_into(
            &mut dst,
            8,
            2,
            2,
            &src(&pixels, 2, 2, fourcc::XRGB8888),
            whole(2, 2),
            whole(2, 2),
        );
        assert_eq!(px(&dst, 8, 0, 0), 0xFFFF_0000);
    }

    /// Plane alpha scales colour as well as alpha.
    ///
    /// Leaving the colour channels alone would produce `rgb > a`, which is not
    /// a valid premultiplied pixel; `blend_pixel_over` assumes it never sees
    /// one, and the visible result is a bright halo at the layer's edges.
    #[test]
    fn plane_alpha_keeps_the_pixel_premultiplied() {
        let mut dst = canvas(2, 2);
        let pixels = solid(2, 2, 0xFFFF_FFFF);
        let mut source = src(&pixels, 2, 2, fourcc::ARGB8888);
        source.plane_alpha = 0x8000;
        blend_into(&mut dst, 8, 2, 2, &source, whole(2, 2), whole(2, 2));

        let out = px(&dst, 8, 0, 0);
        let a = (out >> 24) & 0xFF;
        for shift in [16, 8, 0] {
            assert!(
                (out >> shift) & 0xFF <= a,
                "channel at {shift} exceeds alpha {a:#x} in {out:#010x}"
            );
        }
    }

    /// Fully opaque plane alpha must not darken the source at all.
    ///
    /// `0xFFFF` has to map to exactly `0xFF`; a truncating conversion lands on
    /// `0xFE` and every frame is composited a shade dark.
    #[test]
    fn opaque_plane_alpha_is_a_no_op() {
        let mut dst = canvas(2, 2);
        let pixels = solid(2, 2, 0xFFAB_CDEF);
        blend_into(
            &mut dst,
            8,
            2,
            2,
            &src(&pixels, 2, 2, fourcc::ARGB8888),
            whole(2, 2),
            whole(2, 2),
        );
        assert_eq!(px(&dst, 8, 1, 1), 0xFFAB_CDEF);
    }

    /// A layer partly off the left edge is clipped, not wrapped.
    #[test]
    fn a_negative_destination_clips() {
        let mut dst = canvas(4, 1);
        let pixels = solid(4, 1, 0xFFFF_0000);
        blend_into(
            &mut dst,
            16,
            4,
            1,
            &src(&pixels, 4, 1, fourcc::ARGB8888),
            whole(4, 1),
            CompositeRect {
                x: -2,
                y: 0,
                w: 4,
                h: 1,
            },
        );
        assert_eq!(px(&dst, 16, 0, 0), 0xFFFF_0000);
        assert_eq!(px(&dst, 16, 1, 0), 0xFFFF_0000);
        // The two pixels that fell off the left must not reappear on the right.
        assert_eq!(px(&dst, 16, 2, 0), 0);
        assert_eq!(px(&dst, 16, 3, 0), 0);
    }

    /// A destination entirely off-screen writes nothing.
    #[test]
    fn a_fully_clipped_destination_writes_nothing() {
        let mut dst = solid(4, 4, 0xFF00_00FF);
        let before = dst.clone();
        let pixels = solid(4, 4, 0xFFFF_0000);
        blend_into(
            &mut dst,
            16,
            4,
            4,
            &src(&pixels, 4, 4, fourcc::ARGB8888),
            whole(4, 4),
            CompositeRect {
                x: 100,
                y: 0,
                w: 4,
                h: 4,
            },
        );
        assert_eq!(dst, before);
    }

    /// An unsupported format is refused rather than reinterpreted.
    #[test]
    fn an_unsupported_format_writes_nothing() {
        let mut dst = solid(2, 2, 0xFF00_00FF);
        let before = dst.clone();
        let pixels = solid(2, 2, 0xFFFF_0000);
        blend_into(
            &mut dst,
            8,
            2,
            2,
            &src(&pixels, 2, 2, fourcc::NV12),
            whole(2, 2),
            whole(2, 2),
        );
        assert_eq!(dst, before, "an NV12 source must not be blended as ARGB");
    }

    /// A source buffer shorter than its declared geometry writes nothing.
    ///
    /// The guard is the only thing standing between a mis-declared stride and
    /// an out-of-bounds read, so it is worth a case of its own.
    #[test]
    fn a_short_source_buffer_writes_nothing() {
        let mut dst = solid(4, 4, 0xFF00_00FF);
        let before = dst.clone();
        let pixels = vec![0xFFu8; 8]; // claims 4x4, holds 2 pixels
        blend_into(
            &mut dst,
            16,
            4,
            4,
            &src(&pixels, 4, 4, fourcc::ARGB8888),
            whole(4, 4),
            whole(4, 4),
        );
        assert_eq!(dst, before);
    }

    /// Magnifying picks nearest-neighbour source pixels.
    #[test]
    fn a_smaller_source_scales_up() {
        let mut dst = canvas(4, 1);
        let mut pixels = Vec::new();
        pixels.extend_from_slice(&0xFFFF_0000u32.to_ne_bytes());
        pixels.extend_from_slice(&0xFF00_FF00u32.to_ne_bytes());
        blend_into(
            &mut dst,
            16,
            4,
            1,
            &src(&pixels, 2, 1, fourcc::ARGB8888),
            whole(2, 1),
            whole(4, 1),
        );
        assert_eq!(px(&dst, 16, 0, 0), 0xFFFF_0000);
        assert_eq!(px(&dst, 16, 1, 0), 0xFFFF_0000);
        assert_eq!(px(&dst, 16, 2, 0), 0xFF00_FF00);
        assert_eq!(px(&dst, 16, 3, 0), 0xFF00_FF00);
    }

    /// Clearing a rectangle restores the `SRC_OVER` identity there.
    #[test]
    fn clearing_a_rect_leaves_the_rest_intact() {
        let mut dst = solid(4, 2, 0xFFFF_FFFF);
        clear_into(
            &mut dst,
            16,
            4,
            2,
            CompositeRect {
                x: 1,
                y: 0,
                w: 2,
                h: 1,
            },
        );
        assert_eq!(px(&dst, 16, 0, 0), 0xFFFF_FFFF);
        assert_eq!(px(&dst, 16, 1, 0), 0);
        assert_eq!(px(&dst, 16, 2, 0), 0);
        assert_eq!(px(&dst, 16, 3, 0), 0xFFFF_FFFF);
        assert_eq!(px(&dst, 16, 1, 1), 0xFFFF_FFFF, "row 1 is untouched");
    }

    /// Stacking order is the caller's, and it decides what covers what.
    #[test]
    fn later_blends_land_on_top() {
        let mut dst = canvas(2, 2);
        let under = solid(2, 2, 0xFFFF_0000);
        let over = solid(2, 2, 0xFF00_FF00);
        for pixels in [&under, &over] {
            blend_into(
                &mut dst,
                8,
                2,
                2,
                &src(pixels, 2, 2, fourcc::ARGB8888),
                whole(2, 2),
                whole(2, 2),
            );
        }
        assert_eq!(px(&dst, 8, 0, 0), 0xFF00_FF00);
    }
}
