// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Every decoration blended onto one plane.
//!
//! The middle tier: not enough overlays for a plane each, but one plane free
//! for a canvas. Every decoration is blended into that canvas and the canvas
//! is scanned out whole, so the cost is a canvas-sized blend over whatever
//! changed rather than a plane per window.
//!
//! What makes it affordable is [`compute_damage`](crate::compute_damage): a
//! frame where nothing moved or repainted blends nothing at all.

use crate::{PlaneSlot, PropertyWrite};

/// The property writes that arm the canvas plane.
///
/// The canvas is always **full-screen and unscaled** — it is the size of the
/// CRTC's mode by construction, and its contents are already positioned in
/// screen coordinates. There is no per-decoration geometry here, which is the
/// difference from the plane tier: the positioning happened during the blend.
///
/// `fb_id` is written verbatim, zero included. A zero framebuffer takes the
/// canvas plane down, which is what a caller with nothing to show wants —
/// leaving the previous canvas armed would keep the last frame's decorations
/// on screen after every window has closed.
#[must_use]
pub fn compute_canvas_writes(
    slot: &PlaneSlot,
    fb_id: u32,
    canvas_w: u32,
    canvas_h: u32,
) -> Vec<PropertyWrite> {
    let mut writes = Vec::with_capacity(10);
    let mut add = |property_id: u32, value: u64| {
        writes.push(PropertyWrite {
            object_id: slot.plane_id,
            property_id,
            value,
        });
    };

    add(slot.fb_id_prop, u64::from(fb_id));
    add(slot.crtc_id_prop, u64::from(slot.crtc_id));
    add(slot.crtc_x_prop, 0);
    add(slot.crtc_y_prop, 0);
    add(slot.crtc_w_prop, u64::from(canvas_w));
    add(slot.crtc_h_prop, u64::from(canvas_h));
    add(slot.src_x_prop, 0);
    add(slot.src_y_prop, 0);
    // 16.16 fixed point, as KMS reads SRC_W and SRC_H. Writing pixels here
    // asks for a source region 65536 times too small, which the driver either
    // refuses or scales up from a few pixels.
    add(slot.src_w_prop, u64::from(canvas_w) << 16);
    add(slot.src_h_prop, u64::from(canvas_h) << 16);

    writes
}
