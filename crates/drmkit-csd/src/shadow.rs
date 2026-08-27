// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Drop shadows, and not drawing the same one twice.
//!
//! A shadow is a blurred rounded rectangle. Blurring one is the most
//! expensive thing a decoration does — three box passes over the whole patch,
//! every frame, per window — and the result depends only on the patch size,
//! the elevation, and the theme. So it is cached, and the cache is what makes
//! decorations affordable rather than the blur being fast.

use std::collections::HashMap;

use crate::{Color, Theme};

/// How much shadow a window casts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Elevation {
    /// Unfocused: softer and weaker, so the focused window reads as nearer.
    #[default]
    Blurred,
    /// Focused.
    Focused,
}

/// What identifies one cached shadow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShadowKey {
    /// Patch width in pixels.
    pub width: u32,
    /// Patch height in pixels.
    pub height: u32,
    /// Which elevation.
    pub elevation: Elevation,
    /// Which theme, by [`theme_id`].
    pub theme_id: u64,
}

/// A hash of everything about a theme that changes how a shadow looks.
///
/// Not of the whole theme. The name and the animation duration cannot change
/// a pixel, so hashing them would evict every cached shadow when a caller
/// renamed a theme or made its animations quicker — a cache miss on every
/// window, for nothing.
///
/// FNV-1a, matching upstream, because the value is a cache key rather than a
/// security boundary and both ends must agree on it.
#[must_use]
pub fn theme_id(theme: &Theme) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET;
    let byte = |value: u8, hash: &mut u64| {
        *hash ^= u64::from(value);
        *hash = hash.wrapping_mul(PRIME);
    };
    let u32_field = |value: u32, hash: &mut u64| {
        for shift in [0, 8, 16, 24] {
            byte(u8::try_from((value >> shift) & 0xFF).unwrap_or(0), hash);
        }
    };
    let color = |c: Color, hash: &mut u64| {
        for channel in [c.r, c.g, c.b, c.a] {
            byte(channel, hash);
        }
    };

    u32_field(theme.corner_radius.cast_unsigned(), &mut hash);
    u32_field(theme.shadow_extent.cast_unsigned(), &mut hash);
    for b in theme.noise_amplitude.to_ne_bytes() {
        byte(b, &mut hash);
    }
    u32_field(theme.title_bar.height.cast_unsigned(), &mut hash);
    u32_field(theme.title_bar.font_size.cast_unsigned(), &mut hash);
    for c in [
        theme.colors.panel_top,
        theme.colors.panel_bottom,
        theme.colors.rim_focused,
        theme.colors.rim_blurred,
        theme.colors.shadow,
        theme.colors.title_text,
        theme.colors.title_shadow,
        theme.buttons.close.fill,
        theme.buttons.close.hover,
        theme.buttons.minimize.fill,
        theme.buttons.minimize.hover,
        theme.buttons.maximize.fill,
        theme.buttons.maximize.hover,
    ] {
        color(c, &mut hash);
    }
    hash
}

/// Where a shadow is being blitted to.
#[derive(Debug)]
pub struct ShadowDest<'a> {
    /// The destination pixels, `ARGB8888`.
    pub pixels: &'a mut [u8],
    /// Bytes between rows.
    pub stride: u32,
    /// Region width in pixels.
    pub width: u32,
    /// Region height in pixels.
    pub height: u32,
}

/// How many shadows to keep when the caller does not say.
pub const DEFAULT_CAPACITY: usize = 8;

/// Rendered shadows, most recently used last.
pub struct ShadowCache {
    /// Patch pixels by key, `ARGB8888` premultiplied.
    entries: HashMap<ShadowKey, Vec<u8>>,
    /// Use order, oldest first. Separate from the map because a `HashMap` has
    /// no order and the eviction needs one.
    order: Vec<ShadowKey>,
    capacity: usize,
}

impl std::fmt::Debug for ShadowCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowCache")
            .field("cached", &self.entries.len())
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl Default for ShadowCache {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl ShadowCache {
    /// A cache holding `capacity` shadows.
    ///
    /// Zero is treated as [`DEFAULT_CAPACITY`]: a cache that held nothing
    /// would re-blur every shadow every frame, which is the cost this type
    /// exists to remove.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
            capacity: if capacity == 0 {
                DEFAULT_CAPACITY
            } else {
                capacity
            },
        }
    }

    /// How many shadows are cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many it will hold.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Whether this shadow is already rendered.
    #[must_use]
    pub fn contains(&self, key: &ShadowKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Forget everything.
    ///
    /// For a theme change that invalidates every shadow at once, where
    /// evicting one at a time would blur each of them again on the way out.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    /// Blit this shadow into `dst`, rendering it first if it is not cached.
    ///
    /// Returns whether anything was written. A zero-sized key or an
    /// undersized destination writes nothing rather than failing: a window
    /// being resized passes through both, and a decoration that errored on
    /// the way would flicker.
    ///
    /// A destination smaller than the patch is **clipped**, not refused — the
    /// alternative is reading past the caller's buffer.
    pub fn blit_into(&mut self, key: ShadowKey, theme: &Theme, dst: &mut ShadowDest<'_>) -> bool {
        if key.width == 0 || key.height == 0 || dst.width == 0 || dst.height == 0 {
            return false;
        }
        self.ensure(key, theme);
        let Some(patch) = self.entries.get(&key) else {
            return false;
        };

        let rows = key.height.min(dst.height) as usize;
        let cols = key.width.min(dst.width) as usize;
        for y in 0..rows {
            let from = y * key.width as usize * 4;
            let to = y * dst.stride as usize;
            let (Some(src), Some(out)) = (
                patch.get(from..from + cols * 4),
                dst.pixels.get_mut(to..to + cols * 4),
            ) else {
                // A destination whose stride does not match its own height is
                // the caller's error, but stopping short beats writing past
                // the end of their buffer.
                break;
            };
            out.copy_from_slice(src);
        }
        true
    }

    /// Blit a blend of two shadows, `t` of the way from `a` to `b`.
    ///
    /// What a focus change animates through. Both patches are cached, so the
    /// transition costs a per-pixel lerp rather than a blur per frame.
    ///
    /// `t` is clamped to `0..=1`: an animation that overshoots is a caller
    /// bug, and extrapolating a colour past its endpoints produces values
    /// that are not a shadow at all.
    pub fn blit_cross_fade(
        &mut self,
        a: ShadowKey,
        b: ShadowKey,
        theme: &Theme,
        dst: &mut ShadowDest<'_>,
        t: f32,
    ) -> bool {
        if a.width == 0 || a.height == 0 || dst.width == 0 || dst.height == 0 {
            return false;
        }
        self.ensure(a, theme);
        self.ensure(b, theme);
        let (Some(from), Some(to)) = (self.entries.get(&a), self.entries.get(&b)) else {
            return false;
        };
        let t = t.clamp(0.0, 1.0);

        let rows = a.height.min(b.height).min(dst.height) as usize;
        let cols = a.width.min(b.width).min(dst.width) as usize;
        for y in 0..rows {
            for x in 0..cols * 4 {
                let src_index = y * a.width as usize * 4 + x;
                let dst_index = y * dst.stride as usize + x;
                let (Some(lhs), Some(rhs), Some(out)) = (
                    from.get(src_index),
                    to.get(y * b.width as usize * 4 + x),
                    dst.pixels.get_mut(dst_index),
                ) else {
                    break;
                };
                let blended = f32::from(*lhs) + (f32::from(*rhs) - f32::from(*lhs)) * t;
                *out = to_u8(blended);
            }
        }
        true
    }

    /// Render this shadow if it is not cached, and mark it most recent.
    fn ensure(&mut self, key: ShadowKey, theme: &Theme) {
        if self.entries.contains_key(&key) {
            // A hit still counts as a use, or the LRU would evict whatever the
            // caller is drawing every frame and keep whatever it drew once.
            self.order.retain(|existing| *existing != key);
            self.order.push(key);
            return;
        }

        while self.entries.len() >= self.capacity && !self.order.is_empty() {
            let oldest = self.order.remove(0);
            self.entries.remove(&oldest);
        }

        self.entries.insert(key, render_shadow(key, theme));
        self.order.push(key);
    }
}

/// Round a clamped 0..=255 float to a byte.
///
/// The clamp is the precondition: `as u8` on an unclamped float saturates on
/// some targets and wraps on others, and a shadow is not worth a difference
/// like that between architectures.
fn to_u8(value: f32) -> u8 {
    // `round` rather than truncate: truncating biases every channel down by
    // up to one, which over a whole blurred patch is a visibly lighter shadow.
    // Stepping through the 256 byte values is exact and has no cast in it,
    // which is worth more here than the arithmetic would be: the range is
    // fixed and tiny, and every float-to-integer cast in Rust has a
    // target-dependent edge this avoids entirely.
    let clamped = value.clamp(0.0, 255.0);
    u8::try_from(
        (0u16..=255)
            .find(|step| f32::from(*step) >= clamped - 0.5)
            .unwrap_or(255),
    )
    .unwrap_or(u8::MAX)
}

/// Render one shadow patch: a rounded rectangle, blurred, in the theme's
/// shadow colour.
fn render_shadow(key: ShadowKey, theme: &Theme) -> Vec<u8> {
    let w = key.width as usize;
    let h = key.height as usize;
    let extent = theme.shadow_extent.max(0);

    // The mask is the shape before it is blurred: opaque inside the panel,
    // clear outside. The blur is what turns the edge into a shadow.
    let mut mask = vec![0u8; w * h];
    let inset = 2 * u32::try_from(extent).unwrap_or(0);
    let panel_w = key.width.saturating_sub(inset) as usize;
    let panel_h = key.height.saturating_sub(inset) as usize;
    let radius = usize::try_from(theme.corner_radius.max(0))
        .unwrap_or(0)
        .min(panel_w / 2)
        .min(panel_h / 2);

    if panel_w > 0 && panel_h > 0 {
        let origin = usize::try_from(extent).unwrap_or(0);
        for y in 0..h {
            for x in 0..w {
                if inside_rounded_rect(x, y, origin, origin, panel_w, panel_h, radius) {
                    mask[y * w + x] = 0xFF;
                }
            }
        }
    }

    blur3(&mut mask, w, h, usize::try_from(extent).unwrap_or(0));

    // Premultiplied, because that is what a blit into an ARGB8888 buffer
    // composites correctly without a per-pixel divide.
    let shadow = theme.colors.shadow;
    let intensity = if key.elevation == Elevation::Focused {
        1.0
    } else {
        // An unfocused window casts a weaker shadow, which is what makes the
        // focused one read as nearer.
        0.7
    };

    let mut out = vec![0u8; w * h * 4];
    for (index, coverage) in mask.iter().enumerate() {
        let alpha = f32::from(*coverage) / 255.0 * f32::from(shadow.a) * intensity;
        let a8 = to_u8(alpha);
        // The product of two bytes over 255 is a byte by construction, so
        // this cannot lose anything -- but saying so with try_from keeps the
        // arithmetic checkable rather than asserted in a comment.
        let premul = |channel: u8| {
            u8::try_from((u32::from(channel) * u32::from(a8)) / 255).unwrap_or(u8::MAX)
        };
        let offset = index * 4;
        out[offset] = premul(shadow.b);
        out[offset + 1] = premul(shadow.g);
        out[offset + 2] = premul(shadow.r);
        out[offset + 3] = a8;
    }
    out
}

/// Whether a point is inside a rounded rectangle.
fn inside_rounded_rect(
    x: usize,
    y: usize,
    rect_x: usize,
    rect_y: usize,
    w: usize,
    h: usize,
    radius: usize,
) -> bool {
    if x < rect_x || y < rect_y || x >= rect_x + w || y >= rect_y + h {
        return false;
    }
    if radius == 0 {
        return true;
    }
    let (lx, ly) = (x - rect_x, y - rect_y);
    // Only the four corner squares can be outside; everything else is in.
    let (cx, cy) = (
        if lx < radius {
            radius
        } else if lx >= w - radius {
            w - radius - 1
        } else {
            return true;
        },
        if ly < radius {
            radius
        } else if ly >= h - radius {
            h - radius - 1
        } else {
            return true;
        },
    );
    // Integer arithmetic throughout: the distance test is exact, and a
    // corner that rounds differently on one target than another would put a
    // pixel of shadow in a different place per architecture.
    let dx = lx.abs_diff(cx);
    let dy = ly.abs_diff(cy);
    dx * dx + dy * dy <= radius * radius
}

/// Three box blurs, which approximate a Gaussian closely enough for a shadow
/// and cost a fraction of one.
///
/// Three is the standard number: one box is visibly square, two are still
/// boxy at the corners, and the third is where the error stops being visible.
fn blur3(buf: &mut [u8], width: usize, height: usize, total_radius: usize) {
    if total_radius == 0 || width == 0 || height == 0 {
        return;
    }
    let per_pass = (total_radius / 3).max(1);
    let mut scratch = vec![0u8; buf.len()];
    for _ in 0..3 {
        box_blur_h(buf, &mut scratch, width, height, per_pass);
        box_blur_v(&scratch, buf, width, height, per_pass);
    }
}

/// One horizontal box pass.
fn box_blur_h(src: &[u8], dst: &mut [u8], width: usize, height: usize, radius: usize) {
    let window = u32::try_from(2 * radius + 1).unwrap_or(1).max(1);
    for y in 0..height {
        let row = y * width;
        let mut sum: u32 = 0;
        // Seed with the left edge clamped, so the blur does not darken at the
        // borders as though the image were surrounded by transparency.
        for x in 0..=radius.min(width - 1) {
            sum += u32::from(src[row + x]);
        }
        sum += u32::from(src[row]) * u32::try_from(radius).unwrap_or(0);
        for x in 0..width {
            dst[row + x] = u8::try_from(sum / window).unwrap_or(u8::MAX);
            let next = src[row + (x + radius + 1).min(width - 1)];
            let gone = src[row + x.saturating_sub(radius)];
            sum = sum + u32::from(next) - u32::from(gone);
        }
    }
}

/// One vertical box pass.
fn box_blur_v(src: &[u8], dst: &mut [u8], width: usize, height: usize, radius: usize) {
    let window = u32::try_from(2 * radius + 1).unwrap_or(1).max(1);
    for x in 0..width {
        let mut sum: u32 = 0;
        for y in 0..=radius.min(height - 1) {
            sum += u32::from(src[y * width + x]);
        }
        sum += u32::from(src[x]) * u32::try_from(radius).unwrap_or(0);
        for y in 0..height {
            dst[y * width + x] = u8::try_from(sum / window).unwrap_or(u8::MAX);
            let next = src[(y + radius + 1).min(height - 1) * width + x];
            let gone = src[y.saturating_sub(radius) * width + x];
            sum = sum + u32::from(next) - u32::from(gone);
        }
    }
}
