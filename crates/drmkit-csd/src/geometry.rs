// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Where the parts of a decoration go.

use crate::Theme;

/// The button radius, in pixels.
///
/// Fixed rather than themed. The three buttons are a fixed idiom — a user
/// learns where they are once — and a theme that could resize them could put
/// them outside the title bar it also sized.
const BUTTON_RADIUS: i32 = 7;
/// Gap between adjacent buttons.
const BUTTON_GAP: i32 = 6;
/// Gap between the rightmost button and the panel's right edge.
const BUTTON_RIGHT_PAD: i32 = 10;

/// Where everything sits inside a decoration of a given size.
///
/// All in decoration-local pixels, with the origin at the top left of the
/// **decoration**, not of the window — the shadow is drawn outside the panel,
/// so the panel starts inset by the shadow's extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecorationGeometry {
    /// Panel left edge.
    pub panel_x: i32,
    /// Panel top edge.
    pub panel_y: i32,
    /// Panel width.
    pub panel_w: i32,
    /// Panel height.
    pub panel_h: i32,
    /// Title bar height, measured from the panel's top.
    pub title_bar_height: i32,
    /// Radius of each window button.
    pub button_radius: i32,
    /// Vertical centre of every button.
    pub button_cy: i32,
    /// Horizontal centre of the close button.
    pub close_cx: i32,
    /// Of the minimize button.
    pub minimize_cx: i32,
    /// Of the maximize button.
    pub maximize_cx: i32,
}

/// Lay out a decoration `deco_w` by `deco_h` under `theme`.
///
/// Never fails and never panics: a decoration too small to hold a panel gets
/// a zero-sized one rather than a negative one. A negative width reaching a
/// rasterizer is either a crash or a very large unsigned number, and the
/// caller that asked for a 4-pixel decoration under a 24-pixel shadow has
/// made an ordinary mistake.
#[must_use]
pub fn decoration_geometry(theme: &Theme, deco_w: u32, deco_h: u32) -> DecorationGeometry {
    // A negative extent in a hand-written theme means none, not an inset the
    // other way -- a panel drawn outside its own decoration is not a thing to
    // reproduce faithfully.
    let extent = theme.shadow_extent.max(0);

    let panel_w = (i64::from(deco_w) - i64::from(2 * extent)).max(0);
    let panel_h = (i64::from(deco_h) - i64::from(2 * extent)).max(0);
    let panel_w = i32::try_from(panel_w).unwrap_or(i32::MAX);
    let panel_h = i32::try_from(panel_h).unwrap_or(i32::MAX);

    let title_bar_height = theme.title_bar.height.max(0);
    let button_cy = extent + (title_bar_height / 2);

    // Right to left: close is outermost, because it is the one a user reaches
    // for by feel and the one that must not move when the others are absent.
    let right_edge = extent
        .saturating_add(panel_w)
        .saturating_sub(BUTTON_RIGHT_PAD);
    let close_cx = right_edge - BUTTON_RADIUS;
    let step = (2 * BUTTON_RADIUS) + BUTTON_GAP;

    DecorationGeometry {
        panel_x: extent,
        panel_y: extent,
        panel_w,
        panel_h,
        title_bar_height,
        button_radius: BUTTON_RADIUS,
        button_cy,
        close_cx,
        minimize_cx: close_cx - step,
        maximize_cx: close_cx - (2 * step),
    }
}
