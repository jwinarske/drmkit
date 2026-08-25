// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Writing a cursor's position onto its plane.

use drmkit_core::{AtomicRequest, PropertyStore};

use crate::CursorError;
use crate::sprite::Rotation;

/// Where the plane's top-left corner goes, given where the pointer is.
///
/// The hotspot is the pixel that has to land under the pointer -- the tip of
/// the arrow, the centre of a crosshair -- so the plane is offset back by it.
/// Getting this wrong is invisible in a screenshot and obvious the moment
/// anyone clicks: the sprite looks right and the click lands somewhere else.
///
/// The result is signed and may be negative, which is correct: a cursor near
/// the top-left of the screen has its buffer hanging off the edge.
#[must_use]
pub fn plane_origin(pointer: (i32, i32), hotspot: (u32, u32)) -> (i32, i32) {
    let back = |coordinate: i32, offset: u32| {
        i32::try_from(offset).map_or(coordinate, |offset| coordinate.saturating_sub(offset))
    };
    (back(pointer.0, hotspot.0), back(pointer.1, hotspot.1))
}

/// What to write to the plane's `rotation` property.
///
/// The requested rotation when the plane advertises it, and `ROTATE_0`
/// otherwise -- because in that case the pixels were already turned in
/// software, and asking the hardware to turn them again would turn them twice.
///
/// Written every commit rather than when it changes: a plane that went dark
/// during a hide, or a descriptor replaced across a session resume, has lost
/// whatever the kernel knew, and re-arming is cheaper than tracking that.
#[must_use]
pub fn hardware_rotation(requested: Rotation, supported_mask: u64) -> u64 {
    let want = requested.drm_mask();
    if supported_mask & want != 0 {
        want
    } else {
        Rotation::None.drm_mask()
    }
}

/// The state a cursor plane is put into for one frame.
///
/// Distinct from [`crate::sprite::Placement`], which says where the sprite
/// landed inside the buffer. This says where the buffer lands on the screen.
#[derive(Debug, Clone, Copy)]
pub struct PlaneState {
    /// The plane being written.
    pub plane_id: u32,
    /// The CRTC it feeds.
    pub crtc_id: u32,
    /// The framebuffer to scan out.
    pub fb_id: u32,
    /// Buffer dimensions. Source and destination match: a cursor plane is not
    /// asked to scale.
    pub width: u32,
    /// As [`PlaneState::width`].
    pub height: u32,
    /// Where the pointer is on the CRTC.
    pub pointer_x: i32,
    /// As [`PlaneState::pointer_x`].
    pub pointer_y: i32,
    /// The hotspot in buffer coordinates, from
    /// [`compose`](crate::sprite::compose).
    ///
    /// `None` before a cursor has been drawn, which is a real state: the
    /// buffer exists so the size can be probed, but nothing is in it yet.
    pub hotspot: Option<(u32, u32)>,
    /// How the sprite is turned.
    pub rotation: Rotation,
    /// The rotations the plane can do in hardware, as a `DRM_MODE_ROTATE_*`
    /// mask. Zero when it has no `rotation` property.
    ///
    /// Supplied rather than read back from the property, because `rotation` is
    /// a bitmask and a property store reports ranges. Guessing here would make
    /// every rotation look supported and turn the sprite twice: once in
    /// software, once by the hardware.
    pub rotation_mask: u64,
}

/// Write `placement` into `request`.
///
/// Every property, every commit. Drivers accept partial writes, but a plane
/// that went dark during a hide needs all of it re-armed to come back, and
/// writing the lot is cheaper than tracking which fields a given transition
/// dirtied.
///
/// `rotation`, `HOTSPOT_X` and `HOTSPOT_Y` are optional and skipped when the
/// plane does not carry them. The hotspot properties are a hint for
/// virtualized drivers, which need to know where inside the sprite the pointer
/// actually is; before anything has been drawn they are left alone rather than
/// clobbered with a stale zero.
///
/// Returns how many properties were written.
///
/// # Errors
///
/// [`CursorError::MissingProperty`] if the plane lacks one of the properties
/// every plane is required to have. That is a broken driver rather than
/// anything a caller did.
pub fn stage(
    request: &mut AtomicRequest,
    props: &PropertyStore,
    placement: &PlaneState,
) -> Result<usize, CursorError> {
    let (x, y) = plane_origin(
        (placement.pointer_x, placement.pointer_y),
        placement.hotspot.unwrap_or((0, 0)),
    );

    let mut written = 0;
    let mut put = |name: &str, value: u64| -> Result<(), CursorError> {
        let id = props
            .property_id(placement.plane_id, name)
            .map_err(|_| CursorError::MissingProperty)?;
        request
            .add_property(placement.plane_id, id, value)
            .map_err(|_| CursorError::MissingProperty)?;
        written += 1;
        Ok(())
    };

    put("FB_ID", u64::from(placement.fb_id))?;
    put("CRTC_ID", u64::from(placement.crtc_id))?;
    // Signed, carried in an unsigned slot as two's complement -- which is what
    // the kernel reads back out of it.
    put("CRTC_X", u64::from(x.cast_unsigned()))?;
    put("CRTC_Y", u64::from(y.cast_unsigned()))?;
    put("CRTC_W", u64::from(placement.width))?;
    put("CRTC_H", u64::from(placement.height))?;
    put("SRC_X", 0)?;
    put("SRC_Y", 0)?;
    // The source rectangle is 16.16 fixed point; the CRTC rectangle is plain
    // pixels. Sending either in the other's units is a cursor that fills the
    // screen or vanishes.
    put("SRC_W", u64::from(placement.width) << 16)?;
    put("SRC_H", u64::from(placement.height) << 16)?;

    if let Ok(id) = props.property_id(placement.plane_id, "rotation") {
        let value = hardware_rotation(placement.rotation, placement.rotation_mask);
        if request.add_property(placement.plane_id, id, value).is_err() {
            return Err(CursorError::MissingProperty);
        }
        written += 1;
    }

    if let Some((hotspot_x, hotspot_y)) = placement.hotspot {
        for (name, value) in [("HOTSPOT_X", hotspot_x), ("HOTSPOT_Y", hotspot_y)] {
            if let Ok(id) = props.property_id(placement.plane_id, name) {
                if request
                    .add_property(placement.plane_id, id, u64::from(value))
                    .is_err()
                {
                    return Err(CursorError::MissingProperty);
                }
                written += 1;
            }
        }
    }

    Ok(written)
}
