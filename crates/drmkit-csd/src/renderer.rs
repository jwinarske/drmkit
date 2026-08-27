// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Drawing a decoration.
//!
//! # No rasterizer, deliberately
//!
//! Upstream draws through `Blend2D`. This does not, and takes no rendering
//! dependency at all: a decoration is a rounded rectangle, a one-pixel rim,
//! three circles and a blur, and every one of those is a short loop over
//! bytes. Pulling a general rasterizer in to draw four shapes would be a
//! large dependency for a small amount of arithmetic, and the arithmetic is
//! the part worth being able to read.
//!
//! # No text
//!
//! The title is not drawn. Text needs a font stack — loading, shaping,
//! hinting — which is a real dependency and a separate decision from the
//! shapes. [`Renderer::has_font`] answers `false` and the title bar is drawn
//! without its title, which is what upstream also does when no system font is
//! found.

use crate::{
    Color, DecorationGeometry, Elevation, HoverButton, ShadowCache, ShadowDest, ShadowKey, Theme,
    WindowState, decoration_geometry, theme_id,
};

/// Where a decoration is being drawn.
#[derive(Debug)]
pub struct Canvas<'a> {
    /// `ARGB8888` pixels.
    pub pixels: &'a mut [u8],
    /// Bytes between rows.
    pub stride: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Why a decoration could not be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DrawError {
    /// The canvas has no pixels.
    ///
    /// A window mid-resize passes through zero, and refusing here is better
    /// than a loop that writes nothing and reports success — the caller would
    /// present an unpainted buffer believing it had been drawn.
    #[error("the canvas is empty")]
    EmptyCanvas,

    /// The canvas is smaller than its own stride says.
    #[error("a {width}x{height} canvas at stride {stride} needs {needed} bytes, got {got}")]
    Undersized {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
        /// Declared stride.
        stride: u32,
        /// What that implies.
        needed: usize,
        /// What was given.
        got: usize,
    },
}

/// How to build a renderer.
#[derive(Debug, Clone, Default)]
pub struct RendererConfig {
    /// A font to load for the title.
    ///
    /// Accepted and unused: see the module docs. Kept so a caller written
    /// against the reference compiles, and so the field exists when text
    /// arrives.
    pub font_path: Option<String>,
}

/// Draws decorations.
#[derive(Debug, Default)]
pub struct Renderer {
    config: RendererConfig,
}

impl Renderer {
    /// A renderer.
    #[must_use]
    pub fn new(config: RendererConfig) -> Self {
        Self { config }
    }

    /// Whether a font is loaded, and so whether the title will be drawn.
    ///
    /// Always `false`. Text is not implemented — see the module docs — and
    /// answering `true` would have a caller reserve title-bar width for
    /// something that never appears.
    #[must_use]
    pub const fn has_font(&self) -> bool {
        false
    }

    /// The configured font path, whether or not anything uses it.
    #[must_use]
    pub fn font_path(&self) -> Option<&str> {
        self.config.font_path.as_deref()
    }

    /// Draw a decoration for `state` onto `canvas`.
    ///
    /// Deterministic: the same theme, state and size produce byte-identical
    /// output. That is what lets a caller skip a redraw it knows would change
    /// nothing, and it is why the panel's dither is a hash of the coordinate
    /// rather than a random number.
    ///
    /// # Errors
    ///
    /// [`DrawError::EmptyCanvas`] for a zero-sized canvas, and
    /// [`DrawError::Undersized`] if the buffer is shorter than its own stride
    /// and height describe.
    pub fn draw(
        &self,
        theme: &Theme,
        state: &WindowState,
        canvas: &mut Canvas<'_>,
        shadows: &mut ShadowCache,
    ) -> Result<(), DrawError> {
        if canvas.width == 0 || canvas.height == 0 || canvas.pixels.is_empty() {
            return Err(DrawError::EmptyCanvas);
        }
        let needed = canvas.stride as usize * canvas.height as usize;
        if canvas.pixels.len() < needed {
            return Err(DrawError::Undersized {
                width: canvas.width,
                height: canvas.height,
                stride: canvas.stride,
                needed,
                got: canvas.pixels.len(),
            });
        }

        canvas.pixels[..needed].fill(0);

        let geometry = decoration_geometry(theme, canvas.width, canvas.height);
        // Focus drives the shadow's elevation as well as the rim: a settled
        // window uses one patch, and a window mid-transition blends the two
        // rather than blurring a third.
        let focus = focus_amount(state);
        Self::draw_shadow(theme, canvas, shadows, focus);
        draw_panel(theme, canvas, &geometry);
        draw_rim(theme, canvas, &geometry, focus);
        draw_buttons(theme, canvas, &geometry, state);
        Ok(())
    }

    /// Blit the shadow, cross-fading where focus is mid-transition.
    fn draw_shadow(theme: &Theme, canvas: &mut Canvas<'_>, shadows: &mut ShadowCache, focus: f32) {
        if theme.shadow_extent <= 0 || theme.colors.shadow.a == 0 {
            return;
        }
        let id = theme_id(theme);
        let key = |elevation| ShadowKey {
            width: canvas.width,
            height: canvas.height,
            elevation,
            theme_id: id,
        };
        let mut dest = ShadowDest {
            pixels: canvas.pixels,
            stride: canvas.stride,
            width: canvas.width,
            height: canvas.height,
        };

        // Settled either way uses one cached patch; only a transition pays the
        // per-pixel blend, and even then both endpoints are cached.
        if focus <= 0.0 {
            shadows.blit_into(key(Elevation::Blurred), theme, &mut dest);
        } else if focus >= 1.0 {
            shadows.blit_into(key(Elevation::Focused), theme, &mut dest);
        } else {
            shadows.blit_cross_fade(
                key(Elevation::Blurred),
                key(Elevation::Focused),
                theme,
                &mut dest,
                focus,
            );
        }
    }
}

/// How focused the window is, `0..=1`.
///
/// The animator's progress where it has written one, and the boolean
/// otherwise — a caller that never animates still gets a focused window drawn
/// focused rather than stuck at the unset sentinel.
fn focus_amount(state: &WindowState) -> f32 {
    if state.focus_progress < 0.0 {
        if state.focused { 1.0 } else { 0.0 }
    } else {
        state.focus_progress.clamp(0.0, 1.0)
    }
}

/// Fill the panel: a vertical gradient, dithered.
fn draw_panel(theme: &Theme, canvas: &mut Canvas<'_>, geometry: &DecorationGeometry) {
    if geometry.panel_w <= 0 || geometry.panel_h <= 0 {
        return;
    }
    let radius = corner_radius(theme, geometry);
    let (top, bottom) = (theme.colors.panel_top, theme.colors.panel_bottom);
    let height = geometry.panel_h.max(1);

    for row in 0..geometry.panel_h {
        // A vertical gradient, so the mix is per row rather than per pixel.
        let t = f32::from(u16::try_from(row).unwrap_or(0))
            / f32::from(u16::try_from(height).unwrap_or(1));
        let base = lerp_color(top, bottom, t);

        for column in 0..geometry.panel_w {
            if !in_rounded_rect(column, row, geometry.panel_w, geometry.panel_h, radius) {
                continue;
            }
            let x = geometry.panel_x + column;
            let y = geometry.panel_y + row;
            // Dither breaks the banding a flat 8-bit gradient shows across a
            // wide title bar. Derived from the coordinate, not from a random
            // number, so the same decoration draws identically every time.
            let color = dither(base, x, y, theme.noise_amplitude);
            blend_pixel(canvas, x, y, color);
        }
    }
}

/// Stroke the one-pixel rim, between the blurred and focused colours.
fn draw_rim(theme: &Theme, canvas: &mut Canvas<'_>, geometry: &DecorationGeometry, focus: f32) {
    if geometry.panel_w <= 0 || geometry.panel_h <= 0 {
        return;
    }
    let radius = corner_radius(theme, geometry);
    let color = lerp_color(theme.colors.rim_blurred, theme.colors.rim_focused, focus);

    for row in 0..geometry.panel_h {
        for column in 0..geometry.panel_w {
            // The rim is where the shape is, but one pixel further in is not:
            // that is the outline, without needing a second shape to subtract.
            let inside = in_rounded_rect(column, row, geometry.panel_w, geometry.panel_h, radius);
            if !inside {
                continue;
            }
            let interior = in_rounded_rect(column, row, geometry.panel_w, geometry.panel_h, radius)
                && in_inset_rounded_rect(column, row, geometry.panel_w, geometry.panel_h, radius);
            if interior {
                continue;
            }
            blend_pixel(
                canvas,
                geometry.panel_x + column,
                geometry.panel_y + row,
                color,
            );
        }
    }
}

/// Fill the three window buttons.
fn draw_buttons(
    theme: &Theme,
    canvas: &mut Canvas<'_>,
    geometry: &DecorationGeometry,
    state: &WindowState,
) {
    if geometry.title_bar_height <= 0 {
        return;
    }
    let hover_amount = if state.hover_progress < 0.0 {
        1.0
    } else {
        state.hover_progress.clamp(0.0, 1.0)
    };

    for (button, cx, theme_button) in [
        (HoverButton::Close, geometry.close_cx, theme.buttons.close),
        (
            HoverButton::Minimize,
            geometry.minimize_cx,
            theme.buttons.minimize,
        ),
        (
            HoverButton::Maximize,
            geometry.maximize_cx,
            theme.buttons.maximize,
        ),
    ] {
        // Only the hovered button lights, and only as far as the animation has
        // got. Lighting every button on any hover would be worse than lighting
        // none.
        let lit = if state.hover == button {
            hover_amount
        } else {
            0.0
        };
        let color = lerp_color(theme_button.fill, theme_button.hover, lit);
        fill_circle(
            canvas,
            cx,
            geometry.button_cy,
            geometry.button_radius,
            color,
        );
    }
}

/// The corner radius, never more than half the shorter side.
///
/// A radius past that describes a shape whose corners overlap, which is not a
/// rounded rectangle and which the coverage test cannot answer sensibly.
fn corner_radius(theme: &Theme, geometry: &DecorationGeometry) -> i32 {
    theme
        .corner_radius
        .max(0)
        .min(geometry.panel_w / 2)
        .min(geometry.panel_h / 2)
}

/// Whether a point is inside a rounded rectangle of this size.
fn in_rounded_rect(x: i32, y: i32, w: i32, h: i32, radius: i32) -> bool {
    if x < 0 || y < 0 || x >= w || y >= h {
        return false;
    }
    if radius <= 0 {
        return true;
    }
    let cx = if x < radius {
        radius
    } else if x >= w - radius {
        w - radius - 1
    } else {
        return true;
    };
    let cy = if y < radius {
        radius
    } else if y >= h - radius {
        h - radius - 1
    } else {
        return true;
    };
    let (dx, dy) = ((x - cx).unsigned_abs(), (y - cy).unsigned_abs());
    dx * dx + dy * dy <= radius.unsigned_abs() * radius.unsigned_abs()
}

/// Whether a point is inside the same shape shrunk by one pixel.
fn in_inset_rounded_rect(x: i32, y: i32, w: i32, h: i32, radius: i32) -> bool {
    if w <= 2 || h <= 2 {
        return false;
    }
    in_rounded_rect(x - 1, y - 1, w - 2, h - 2, (radius - 1).max(0))
}

/// Fill a circle.
fn fill_circle(canvas: &mut Canvas<'_>, cx: i32, cy: i32, radius: i32, color: Color) {
    if radius <= 0 {
        return;
    }
    let r2 = radius.unsigned_abs() * radius.unsigned_abs();
    for y in (cy - radius)..=(cy + radius) {
        for x in (cx - radius)..=(cx + radius) {
            let (dx, dy) = ((x - cx).unsigned_abs(), (y - cy).unsigned_abs());
            if dx * dx + dy * dy <= r2 {
                blend_pixel(canvas, x, y, color);
            }
        }
    }
}

/// Round a clamped 0..=255 float to a byte, without a float-to-int cast.
///
/// Every such cast in Rust has a target-dependent edge, and a decoration that
/// differed by a channel between architectures would make the determinism
/// this renderer promises untrue in the one place it is hardest to notice.
fn round_channel(value: f64) -> u8 {
    let clamped = value.clamp(0.0, 255.0);
    u8::try_from(
        (0u16..=255)
            .find(|step| f64::from(*step) >= clamped - 0.5)
            .unwrap_or(255),
    )
    .unwrap_or(u8::MAX)
}

/// Mix two colours, `t` of the way from `a` to `b`.
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let mix = |from: u8, to: u8| {
        round_channel(f64::from(from) + (f64::from(to) - f64::from(from)) * f64::from(t))
    };
    Color {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
        a: mix(a.a, b.a),
    }
}

/// Nudge a colour by a coordinate-derived amount.
///
/// The dither. Deterministic by construction: the offset is a hash of the
/// pixel's position, so redrawing the same decoration produces the same
/// bytes, which is what lets a caller skip a redraw that would change nothing.
fn dither(color: Color, x: i32, y: i32, amplitude: f64) -> Color {
    if amplitude <= 0.0 {
        return color;
    }
    // A cheap integer hash of the coordinate, folded to 0..=255.
    let mixed =
        (x.unsigned_abs().wrapping_mul(1_664_525)) ^ (y.unsigned_abs().wrapping_mul(1_013_904_223));
    let noise = (mixed >> 16) & 0xFF;
    // Centred on zero, so the dither does not brighten the panel overall.
    let offset = (f64::from(noise) - 127.5) / 127.5 * amplitude * 255.0;
    let nudge = |channel: u8| round_channel(f64::from(channel) + offset);
    Color {
        r: nudge(color.r),
        g: nudge(color.g),
        b: nudge(color.b),
        a: color.a,
    }
}

/// Source-over one pixel.
fn blend_pixel(canvas: &mut Canvas<'_>, x: i32, y: i32, color: Color) {
    if x < 0 || y < 0 {
        return;
    }
    let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
        return;
    };
    if x >= canvas.width || y >= canvas.height {
        return;
    }
    let offset = (y * canvas.stride + x * 4) as usize;
    let Some(pixel) = canvas.pixels.get_mut(offset..offset + 4) else {
        return;
    };

    let src_a = u32::from(color.a);
    if src_a == 0 {
        return;
    }
    let inverse = 255 - src_a;
    // Source-over on straight alpha: the shadow beneath is already there, and
    // a decoration's panel is translucent by design, so overwriting would lose
    // the shadow the blur just cost.
    let over = |src: u8, dst: u8| {
        let value = (u32::from(src) * src_a + u32::from(dst) * inverse) / 255;
        u8::try_from(value.min(255)).unwrap_or(u8::MAX)
    };
    pixel[0] = over(color.b, pixel[0]);
    pixel[1] = over(color.g, pixel[1]);
    pixel[2] = over(color.r, pixel[2]);
    pixel[3] =
        u8::try_from((src_a + u32::from(pixel[3]) * inverse / 255).min(255)).unwrap_or(u8::MAX);
}
