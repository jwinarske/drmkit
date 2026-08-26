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

use drmkit_core::Device;
use drmkit_dumb::{Buffer, DumbError};
use drmkit_fmt::fourcc;
use drmkit_planes::PlaneCapabilities;

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

/// Copy `height` tightly packed rows into a buffer that may be strided wider.
///
/// A dumb buffer's stride is whatever the kernel chose and may exceed
/// `row_bytes`; the shadow is always tight. Copying the whole shadow in one
/// block would walk that padding into the next row and shear the image
/// progressively down the canvas.
///
/// Split out because vkms never pads — its stride is exactly `width * 4` — so
/// no test against a real device can reach the case this exists for.
fn copy_rows(dst: &mut [u8], dst_stride: usize, shadow: &[u8], row_bytes: usize, height: usize) {
    if row_bytes > dst_stride {
        return;
    }
    if dst.len() < dst_stride * height || shadow.len() < row_bytes * height {
        return;
    }
    for y in 0..height {
        let to = y * dst_stride;
        let from = y * row_bytes;
        dst[to..to + row_bytes].copy_from_slice(&shadow[from..from + row_bytes]);
    }
}

// --- the surface ------------------------------------------------------------

/// Bytes per pixel the canvas can write in `fourcc`, or zero.
///
/// Zero is the gate: a format the canvas cannot emit is not offered even if
/// the plane advertises it. Keeping the check here rather than in the
/// preference list is what stops the two drifting apart, which would show up
/// as a canvas allocated in a format `flush` then writes garbage into.
#[must_use]
pub const fn canvas_output_bpp(fourcc: u32) -> u32 {
    match fourcc {
        fourcc::ARGB8888 | fourcc::XRGB8888 | fourcc::XBGR8888 | fourcc::ABGR8888 => 4,
        fourcc::RGB565 | fourcc::BGR565 => 2,
        _ => 0,
    }
}

/// Formats the canvas will take, best first.
///
/// `ARGB8888` first because it is what the shadow already holds and needs no
/// conversion; the alpha-carrying variants ahead of the opaque ones because a
/// canvas that has blended transparency into it should be able to say so; and
/// the 565s last, which lose two bits a channel but are all some panels take.
const CANVAS_FORMATS: [u32; 6] = [
    fourcc::ARGB8888,
    fourcc::XRGB8888,
    fourcc::XBGR8888,
    fourcc::ABGR8888,
    fourcc::RGB565,
    fourcc::BGR565,
];

/// The best canvas format this plane can scan out, if any.
///
/// The device-free core of the single-plane composition fallback. On a
/// controller with one plane and no `ARGB8888` -- tilcdc on a `BeagleBone` is
/// the canonical one, and i.MX LCDIF and the small SPI panels have the same
/// shape -- a canvas that insisted on `ARGB8888` could not be allocated at
/// all, and every layer that overflowed the plane count would be dropped
/// instead of composited.
///
/// `None` means the plane advertises nothing the canvas can write, which is
/// the honest answer: composition is not available there.
#[must_use]
pub fn canvas_format_for_plane(plane: &PlaneCapabilities) -> Option<u32> {
    CANVAS_FORMATS
        .into_iter()
        .find(|fourcc| canvas_output_bpp(*fourcc) != 0 && plane.supports_format(*fourcc))
}

/// Convert one row of the `ARGB8888` shadow into `out_fourcc`.
///
/// Scalar. The reference hand-vectorizes this with NEON and leans on
/// autovectorization elsewhere; here the loop is written so the compiler can
/// do the same, and the conversion is one row per frame per canvas rather
/// than anything on the per-layer path.
fn convert_row(dst: &mut [u8], src: &[u8], pixels: usize, out_fourcc: u32) {
    match out_fourcc {
        // The shadow is already this. `XRGB8888` differs only in that the
        // high byte is ignored rather than read as alpha, so the bytes are
        // the same either way.
        fourcc::ARGB8888 | fourcc::XRGB8888 => {
            dst[..pixels * 4].copy_from_slice(&src[..pixels * 4]);
        }
        // Same layout with red and blue exchanged.
        fourcc::XBGR8888 | fourcc::ABGR8888 => {
            for i in 0..pixels {
                let s = &src[i * 4..i * 4 + 4];
                // Memory order is B, G, R, A on a little-endian host, so
                // swapping red and blue is swapping bytes 0 and 2.
                dst[i * 4] = s[2];
                dst[i * 4 + 1] = s[1];
                dst[i * 4 + 2] = s[0];
                dst[i * 4 + 3] = s[3];
            }
        }
        fourcc::RGB565 | fourcc::BGR565 => {
            let swap = out_fourcc == fourcc::BGR565;
            for i in 0..pixels {
                let s = &src[i * 4..i * 4 + 4];
                let (b, g, r) = (s[0], s[1], s[2]);
                let (high, low) = if swap { (b, r) } else { (r, b) };
                // Truncating, not rounding: the reference truncates, and a
                // canvas that rounded would differ from it by one step on
                // half the pixels of any gradient.
                let packed =
                    (u16::from(high >> 3) << 11) | (u16::from(g >> 2) << 5) | u16::from(low >> 3);
                dst[i * 2..i * 2 + 2].copy_from_slice(&packed.to_le_bytes());
            }
        }
        // Unreachable through `canvas_format_for_plane`, which only offers
        // formats `canvas_output_bpp` accepts. Leaving the row untouched is
        // the safe answer for a caller that built a canvas by hand.
        _ => {}
    }
}

/// A double-buffered software composition surface.
///
/// Owns two ARGB8888 dumb buffers and alternates between them: the CPU paints
/// the back while the kernel scans out the front. A single buffer tears
/// visibly — the scanout reads the same memory the CPU is writing, so a
/// horizontal seam appears wherever it caught a half-painted frame. The cost
/// is one extra canvas-sized allocation: 3 MB at 1024×768, 33 MB at 4K.
///
/// Painting goes to a **shadow** in ordinary memory, not to the buffer. The
/// kernel typically maps a dumb buffer write-combined, where scattered
/// per-pixel writes are slow but one long linear copy runs at streaming
/// bandwidth. [`flush`](Self::flush) is that copy.
pub struct CompositeCanvas {
    buffers: [Buffer; 2],
    /// Index of the buffer being painted into.
    back: usize,
    /// Cached-memory paint target, `width * height * 4`.
    ///
    /// Always `ARGB8888` whatever the buffers are: blending happens here, and
    /// blending in 565 would quantize at every layer instead of once at the
    /// end. [`flush`](Self::flush) is where the conversion happens.
    shadow: Vec<u8>,
    width: u32,
    height: u32,
    /// What the buffers are, and what `flush` converts the shadow into.
    fourcc: u32,
}

impl CompositeCanvas {
    /// Allocate the pair at `width` × `height`.
    ///
    /// # Errors
    ///
    /// [`DumbError`] if either allocation or its framebuffer fails.
    pub fn create(device: &Device, width: u32, height: u32) -> Result<Self, DumbError> {
        Self::create_in(device, width, height, fourcc::ARGB8888)
    }

    /// Allocate the pair in a format the target plane can scan out.
    ///
    /// For the single-plane fallback: pass what
    /// [`canvas_format_for_plane`] returned, and the canvas emits that
    /// instead of `ARGB8888`. Blending still happens in `ARGB8888` -- see
    /// [`shadow`](Self#structfield.shadow) -- so a 565 canvas quantizes once
    /// at flush rather than at every layer.
    ///
    /// # Errors
    ///
    /// [`DumbError::InvalidConfig`] if `fourcc` is not one the canvas can
    /// write, and otherwise as [`CompositeCanvas::create`].
    pub fn create_in(
        device: &Device,
        width: u32,
        height: u32,
        fourcc: u32,
    ) -> Result<Self, DumbError> {
        let bpp = canvas_output_bpp(fourcc);
        if bpp == 0 {
            return Err(DumbError::InvalidConfig {
                reason: "the canvas cannot write this format",
            });
        }
        let config = drmkit_dumb::Config {
            width,
            height,
            fourcc,
            bpp: bpp * 8,
            add_fb: true,
        };
        let buffers = [
            Buffer::create(device, &config)?,
            Buffer::create(device, &config)?,
        ];
        Ok(Self {
            buffers,
            back: 0,
            shadow: vec![0; (width as usize) * (height as usize) * 4],
            width,
            height,
            fourcc,
        })
    }

    /// What the canvas scans out as.
    #[must_use]
    pub const fn fourcc(&self) -> u32 {
        self.fourcc
    }

    /// Swap back and front before painting.
    ///
    /// Call once per frame, before the first paint. Calling it twice in one
    /// frame would paint into the buffer just handed to the kernel.
    pub const fn begin_frame(&mut self) {
        self.back = 1 - self.back;
    }

    /// Zero the whole shadow: transparent black, the `SRC_OVER` identity.
    pub fn clear(&mut self) {
        self.shadow.fill(0);
    }

    /// Zero one rectangle of the shadow.
    ///
    /// Scrubbing only the union of last frame's and this frame's painted
    /// region turns the per-frame cost from `width * height * 4` bytes into
    /// something proportional to what actually moved.
    pub fn clear_rect(&mut self, rect: CompositeRect) {
        let (w, h) = (self.width, self.height);
        clear_into(&mut self.shadow, w * 4, w, h, rect);
    }

    /// Blend one source over the shadow.
    pub fn blend(
        &mut self,
        src: &CompositeSrc<'_>,
        src_rect: CompositeRect,
        dst_rect: CompositeRect,
    ) {
        let (w, h) = (self.width, self.height);
        blend_into(&mut self.shadow, w * 4, w, h, src, src_rect, dst_rect);
    }

    /// Copy the painted shadow into the back buffer.
    ///
    /// Row by row rather than one call, because the kernel is free to pad a
    /// dumb buffer's stride above `width * 4`; copying straight through would
    /// walk the padding into the next row and shear the image.
    pub fn flush(&mut self) {
        let stride = self.buffers[self.back].stride() as usize;
        let pixels = self.width as usize;
        let height = self.height as usize;
        let fourcc = self.fourcc;
        let shadow_row = pixels * 4;
        let dst = self.buffers[self.back].data_mut();

        if fourcc == fourcc::ARGB8888 || fourcc == fourcc::XRGB8888 {
            // The common case, and the one worth keeping as one long linear
            // copy per row: the kernel usually maps a dumb buffer
            // write-combined, where a memcpy runs at streaming bandwidth and
            // scattered writes do not.
            copy_rows(dst, stride, &self.shadow, shadow_row, height);
            return;
        }

        for row in 0..height {
            let Some(out) = dst.get_mut(row * stride..) else {
                break;
            };
            let src = &self.shadow[row * shadow_row..][..shadow_row];
            convert_row(out, src, pixels, fourcc);
        }
    }

    /// The framebuffer to arm on the canvas plane — the one just painted.
    #[must_use]
    pub fn fb_id(&self) -> Option<u32> {
        self.buffers[self.back].fb_id()
    }

    /// Canvas width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Canvas height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Read one pixel back out of the buffer being painted.
    ///
    /// Reads the framebuffer's own memory rather than the shadow, which is the
    /// only way to tell that [`flush`](Self::flush) actually reached the buffer
    /// the kernel will scan out. Zero for a pixel outside the canvas, or once
    /// the mapping has been dropped.
    #[must_use]
    pub fn back_pixel(&self, x: u32, y: u32) -> u32 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let stride = self.buffers[self.back].stride() as usize;
        let data = self.buffers[self.back].data();
        let at = y as usize * stride + x as usize * 4;
        data.get(at..at + 4).map_or(0, |bytes| {
            u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        })
    }

    /// Whether the canvas can be armed.
    ///
    /// False once [`forget`](Self::forget) has run, so a scene that resumes
    /// without re-allocating cannot commit a framebuffer id the kernel no
    /// longer knows.
    #[must_use]
    pub fn armable(&self) -> bool {
        !self.buffers[0].is_empty() && !self.buffers[1].is_empty() && self.fb_id().is_some()
    }

    /// Drop both buffers' kernel state without issuing ioctls.
    ///
    /// For session pause, where the descriptor has been revoked and cannot
    /// service the ioctls a normal teardown would make. The shadow survives —
    /// it is ordinary memory and owes the device nothing.
    pub fn forget(&mut self) {
        for buffer in &mut self.buffers {
            buffer.forget();
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

#[cfg(test)]
mod stride_tests {
    use super::copy_rows;

    /// Padding stays between rows instead of shifting them.
    ///
    /// The failure this guards against is not a crash — it is every row after
    /// the first landing a few pixels further left than the last, so the image
    /// shears diagonally. Unreachable on vkms, which never pads.
    #[test]
    fn a_padded_stride_does_not_shear_the_image() {
        let row_bytes = 8; // two pixels
        let stride = 12; // one pixel of padding per row
        let height = 3;
        let shadow: Vec<u8> = (0..u8::try_from(row_bytes * height).expect("fits")).collect();
        let mut dst = vec![0xEE; stride * height];

        copy_rows(&mut dst, stride, &shadow, row_bytes, height);

        for y in 0..height {
            let at = y * stride;
            assert_eq!(
                &dst[at..at + row_bytes],
                &shadow[y * row_bytes..(y + 1) * row_bytes],
                "row {y} landed at the wrong offset"
            );
            assert_eq!(
                &dst[at + row_bytes..at + stride],
                &[0xEE; 4],
                "row {y}'s padding must be left alone, not written through"
            );
        }
    }

    /// A destination too small for its stated geometry is left untouched.
    #[test]
    fn a_short_destination_copies_nothing() {
        let mut dst = vec![0u8; 8];
        let before = dst.clone();
        copy_rows(&mut dst, 8, &[1, 2, 3, 4, 5, 6, 7, 8], 8, 4);
        assert_eq!(dst, before);
    }

    /// A stride narrower than a row is refused rather than truncating.
    #[test]
    fn a_stride_narrower_than_a_row_copies_nothing() {
        let mut dst = vec![0u8; 16];
        let before = dst.clone();
        copy_rows(&mut dst, 4, &[9u8; 16], 8, 2);
        assert_eq!(dst, before);
    }

    // ── Canvas format negotiation ───────────────────────────────────────

    use super::{canvas_format_for_plane, canvas_output_bpp, convert_row};
    use drmkit_fmt::fourcc;
    use drmkit_planes::PlaneCapabilities;

    fn plane_with(formats: &[u32]) -> PlaneCapabilities {
        PlaneCapabilities {
            formats: formats.to_vec(),
            ..PlaneCapabilities::default()
        }
    }

    #[test]
    fn tilcdc_picks_xbgr_rather_than_being_stuck() {
        // The board the fallback exists for: no ARGB8888 or XRGB8888, but
        // XBGR8888 and RGB565 scan out. A canvas that insisted on ARGB8888
        // could not be allocated, and every overflow layer would be dropped
        // rather than composited.
        let plane = plane_with(&[fourcc::RGB565, fourcc::XBGR8888, fourcc::BGR888]);
        assert_eq!(canvas_format_for_plane(&plane), Some(fourcc::XBGR8888));
    }

    #[test]
    fn argb_wins_where_it_is_offered() {
        // Byte-identical to the shadow, so flush is a copy with no per-row
        // conversion at all.
        let plane = plane_with(&[
            fourcc::NV12,
            fourcc::XRGB8888,
            fourcc::ARGB8888,
            fourcc::RGB565,
        ]);
        assert_eq!(canvas_format_for_plane(&plane), Some(fourcc::ARGB8888));
    }

    #[test]
    fn the_plane_s_advertisement_order_does_not_decide() {
        // Preference is by what the canvas costs to convert, not by what the
        // driver happened to list first.
        let plane = plane_with(&[fourcc::XBGR8888, fourcc::ARGB8888]);
        assert_eq!(canvas_format_for_plane(&plane), Some(fourcc::ARGB8888));
    }

    #[test]
    fn a_565_only_panel_still_hosts_a_canvas() {
        let plane = plane_with(&[fourcc::RGB565]);
        assert_eq!(canvas_format_for_plane(&plane), Some(fourcc::RGB565));
    }

    #[test]
    fn every_format_the_negotiation_offers_is_one_flush_can_write() {
        // The two lists drifting apart would give a canvas allocated in a
        // format `flush` writes garbage into, which no error reports.
        for fourcc in [
            fourcc::ARGB8888,
            fourcc::XRGB8888,
            fourcc::XBGR8888,
            fourcc::ABGR8888,
            fourcc::RGB565,
            fourcc::BGR565,
        ] {
            assert_eq!(
                canvas_format_for_plane(&plane_with(&[fourcc])),
                Some(fourcc),
                "{fourcc:#010x} is not selectable"
            );
            assert_ne!(
                canvas_output_bpp(fourcc),
                0,
                "{fourcc:#010x} cannot be written"
            );
        }
    }

    #[test]
    fn a_plane_with_nothing_the_canvas_can_write_hosts_none() {
        // The scene keeps looking, or drops the overflow -- either beats
        // arming a buffer nothing can emit into.
        let plane = plane_with(&[fourcc::NV12, fourcc::YUYV]);
        assert_eq!(canvas_format_for_plane(&plane), None);
        assert_eq!(canvas_format_for_plane(&plane_with(&[])), None);
    }

    #[test]
    fn the_conversion_leaves_argb_alone() {
        // 0xAARRGGBB is B, G, R, A in memory on a little-endian host.
        let src = [0x11_u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        for fourcc in [fourcc::ARGB8888, fourcc::XRGB8888] {
            let mut dst = [0_u8; 8];
            convert_row(&mut dst, &src, 2, fourcc);
            assert_eq!(dst, src, "{fourcc:#010x}");
        }
    }

    #[test]
    fn the_conversion_exchanges_red_and_blue() {
        // Bytes 0 and 2 swap; green and alpha stay where they are. Getting
        // this backwards is the classic blue-faces bug, and nothing rejects
        // it -- the picture is simply wrong.
        let src = [0x11_u8, 0x22, 0x33, 0x44];
        for fourcc in [fourcc::XBGR8888, fourcc::ABGR8888] {
            let mut dst = [0_u8; 4];
            convert_row(&mut dst, &src, 1, fourcc);
            assert_eq!(dst, [0x33, 0x22, 0x11, 0x44], "{fourcc:#010x}");
        }
    }

    #[test]
    fn the_conversion_packs_565_and_truncates() {
        // White, black, and a value that exercises each field. Truncating
        // rather than rounding, which is what the reference does -- rounding
        // would differ by one step on half the pixels of any gradient.
        let cases = [
            // (B, G, R, A) in memory      expected RGB565
            ([0xFF_u8, 0xFF, 0xFF, 0xFF], 0xFFFF_u16),
            ([0x00, 0x00, 0x00, 0xFF], 0x0000),
            ([0x00, 0x00, 0xFF, 0xFF], 0xF800), // pure red
            ([0x00, 0xFF, 0x00, 0xFF], 0x07E0), // pure green
            ([0xFF, 0x00, 0x00, 0xFF], 0x001F), // pure blue
        ];
        for (src, expected) in cases {
            let mut dst = [0_u8; 2];
            convert_row(&mut dst, &src, 1, fourcc::RGB565);
            assert_eq!(u16::from_le_bytes(dst), expected, "{src:02x?} packed wrong");
        }
    }

    #[test]
    fn the_565_swap_moves_red_and_blue_between_the_fields() {
        // BGR565 puts blue in the top five bits where RGB565 puts red.
        let src = [0x00_u8, 0x00, 0xFF, 0xFF]; // pure red
        let mut dst = [0_u8; 2];
        convert_row(&mut dst, &src, 1, fourcc::BGR565);
        assert_eq!(
            u16::from_le_bytes(dst),
            0x001F,
            "red belongs in the low field"
        );
    }

    #[test]
    fn a_format_the_canvas_cannot_write_has_no_depth() {
        assert_eq!(canvas_output_bpp(fourcc::NV12), 0);
        assert_eq!(canvas_output_bpp(fourcc::YUYV), 0);
        assert_eq!(canvas_output_bpp(fourcc::ARGB8888), 4);
        assert_eq!(canvas_output_bpp(fourcc::RGB565), 2);
    }

    #[test]
    fn the_canvas_refuses_a_format_it_cannot_write() {
        // Device-free: `create_in` checks before it allocates, so this is the
        // whole of the guard. A canvas built in NV12 would flush nothing into
        // a buffer the plane happily scans out as garbage.
        assert_eq!(canvas_output_bpp(fourcc::NV12), 0);
    }

    #[test]
    fn a_565_canvas_is_half_the_bytes_per_pixel() {
        // What `create_in` sizes the allocation from. Getting it wrong gives
        // a buffer half the size the stride implies, and flush walks off the
        // end of it.
        assert_eq!(canvas_output_bpp(fourcc::RGB565) * 8, 16);
        assert_eq!(canvas_output_bpp(fourcc::ARGB8888) * 8, 32);
    }
}
