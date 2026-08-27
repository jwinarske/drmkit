// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! One decoration per overlay plane.

use crate::{PropertyWrite, SurfaceRef};

/// The property ids for one reserved plane, and what to write to the ones
/// whose values do not come from the surface.
///
/// Ids rather than names because they are resolved once at probe time; a zero
/// id means the plane does not expose that property, which is why the
/// optional ones are checked rather than assumed.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlaneSlot {
    /// The plane.
    pub plane_id: u32,
    /// The CRTC to bind it to when armed.
    pub crtc_id: u32,
    /// `FB_ID`.
    pub fb_id_prop: u32,
    /// `CRTC_ID`.
    pub crtc_id_prop: u32,
    /// `CRTC_X`.
    pub crtc_x_prop: u32,
    /// `CRTC_Y`.
    pub crtc_y_prop: u32,
    /// `CRTC_W`.
    pub crtc_w_prop: u32,
    /// `CRTC_H`.
    pub crtc_h_prop: u32,
    /// `SRC_X`.
    pub src_x_prop: u32,
    /// `SRC_Y`.
    pub src_y_prop: u32,
    /// `SRC_W`.
    pub src_w_prop: u32,
    /// `SRC_H`.
    pub src_h_prop: u32,
    /// `pixel blend mode`, zero where absent.
    pub blend_mode_prop: u32,
    /// What to write to it.
    pub blend_mode_value: u64,
    /// `alpha`, zero where absent.
    pub alpha_prop: u32,
    /// `zpos`, zero where absent.
    pub zpos_prop: u32,
    /// What to write to it.
    pub zpos_value: u64,
}

/// Why the writes could not be computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PlaneError {
    /// More decorations than reserved planes.
    ///
    /// Refused rather than truncated: dropping the surfaces past the end would
    /// leave windows silently undecorated, and the caller reserved the planes
    /// knowing how many it wanted.
    #[error("{surfaces} decorations for {slots} reserved planes")]
    TooManySurfaces {
        /// How many were passed.
        surfaces: usize,
        /// How many planes are reserved.
        slots: usize,
    },
}

/// Convert `surfaces` to a full set of property writes over `slots`.
///
/// Positional: surface `i` goes on slot `i`. A slot with no surface — because
/// there were fewer, or because the surface is empty — is **disarmed**, not
/// left alone. A plane left with last frame's framebuffer goes on scanning it
/// out, so a window that closed would leave its decoration on screen.
///
/// The size is fixed at 16.16 for `SRC_W`/`SRC_H`, which is what KMS expects,
/// and the surface is never scaled: a decoration drawn at one size and
/// stretched to another is a decoration with the wrong corner radius.
///
/// # Errors
///
/// [`PlaneError::TooManySurfaces`] when there are more surfaces than slots.
pub fn compute_writes(
    slots: &[PlaneSlot],
    surfaces: &[SurfaceRef],
) -> Result<Vec<PropertyWrite>, PlaneError> {
    if surfaces.len() > slots.len() {
        return Err(PlaneError::TooManySurfaces {
            surfaces: surfaces.len(),
            slots: slots.len(),
        });
    }

    let mut writes = Vec::with_capacity(slots.len() * 12);
    let mut add = |object_id: u32, property_id: u32, value: u64| {
        writes.push(PropertyWrite {
            object_id,
            property_id,
            value,
        });
    };

    for (index, slot) in slots.iter().enumerate() {
        let armed = surfaces.get(index).copied().filter(|s| s.is_armed());
        let Some(surface) = armed else {
            // Two writes are enough to take a plane down, and writing the rest
            // would be describing geometry for a framebuffer that is not there.
            add(slot.plane_id, slot.fb_id_prop, 0);
            add(slot.plane_id, slot.crtc_id_prop, 0);
            continue;
        };

        add(slot.plane_id, slot.fb_id_prop, u64::from(surface.fb_id));
        add(slot.plane_id, slot.crtc_id_prop, u64::from(slot.crtc_id));
        // Two's complement, which is how KMS reads a signed CRTC_X: a
        // decoration may sit partly off the left or top edge, and the kernel
        // clips it.
        add(
            slot.plane_id,
            slot.crtc_x_prop,
            i64::from(surface.x).cast_unsigned(),
        );
        add(
            slot.plane_id,
            slot.crtc_y_prop,
            i64::from(surface.y).cast_unsigned(),
        );
        add(slot.plane_id, slot.crtc_w_prop, u64::from(surface.width));
        add(slot.plane_id, slot.crtc_h_prop, u64::from(surface.height));
        add(slot.plane_id, slot.src_x_prop, 0);
        add(slot.plane_id, slot.src_y_prop, 0);
        add(slot.plane_id, slot.src_w_prop, to_16_16(surface.width));
        add(slot.plane_id, slot.src_h_prop, to_16_16(surface.height));

        // The optional three. A zero id means the plane does not expose the
        // property, and writing to object zero is an EINVAL that takes the
        // whole commit down rather than just this plane.
        if slot.blend_mode_prop != 0 {
            add(slot.plane_id, slot.blend_mode_prop, slot.blend_mode_value);
        }
        if slot.alpha_prop != 0 {
            // Fully opaque: the decoration's own alpha is in its pixels, and
            // a per-plane alpha on top would fade the whole thing.
            add(slot.plane_id, slot.alpha_prop, 0xFFFF);
        }
        if slot.zpos_prop != 0 {
            add(slot.plane_id, slot.zpos_prop, slot.zpos_value);
        }
    }

    Ok(writes)
}

/// Convert a pixel count to KMS's 16.16 fixed point.
const fn to_16_16(value: u32) -> u64 {
    (value as u64) << 16
}
