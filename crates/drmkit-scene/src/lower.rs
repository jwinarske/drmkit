// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Lowering a scene layer into the plane property bag the allocator reads.

use drmkit_planes::{Layer as PlaneLayer, PropTag};

use crate::display::{DisplayParams, to_16_16};
use crate::source::{BindingModel, SourceFormat};

/// Everything `lower_layer` needs about a layer, without borrowing the scene.
#[derive(Debug, Clone, Copy)]
pub struct LoweringInput {
    /// How to display the buffer.
    pub display: DisplayParams,
    /// What the buffer is.
    pub format: SourceFormat,
    /// Who owns the `FB_ID` write.
    pub binding: BindingModel,
    /// The framebuffer this frame, ignored for a driver-owned binding.
    pub fb_id: u32,
    /// The CRTC this scene drives.
    pub crtc_id: u32,
    /// A stacking position to use when the caller set none.
    ///
    /// The scene derives this from the plane the layer is likely to land on;
    /// `None` leaves `zpos` unwritten entirely.
    pub default_zpos: Option<u64>,
}

/// Copy a scene layer's state into the plane property bag.
///
/// The allocator and the atomic request read from this bag, so every decision
/// here is visible downstream as a placement outcome.
///
/// # Why some properties are conditional
///
/// Three of these were bugs before they were rules, and the C++ records why:
///
/// - **`FB_ID` is skipped for a driver-owned binding.** An EGL stream consumer
///   sets it up through its own extension stack; writing it here races the
///   consumer's internal state. The layer is also flagged externally bound so
///   the allocator skips it entirely.
///
/// - **`CRTC_ID` is always written.** Without it the kernel rejects the plane
///   commit — the framebuffer is armed but the plane is still bound to whatever
///   the previous commit left. Every allocator test commit then fails, and the
///   scene reports zero layers assigned.
///
/// - **`zpos` is written only when asked for.** Emitting 0 unconditionally
///   would statically disqualify any primary plane with an immutable non-zero
///   `zpos` — amdgpu pins primary at 2 — leaving a single-layer scene with
///   nowhere to go.
///
/// And one resolution that is easy to get wrong: a zero-sized `src_rect` means
/// "the buffer's full extent". Callers commonly set only `dst_rect`, because
/// the scene cannot guess a screen position but *can* read the source extent
/// from the format. Without this resolution `SRC_W`/`SRC_H` go out as 0, which
/// the kernel rejects with `EINVAL` — the C++ records this as the root cause of
/// direct scanout failing on every controller it was tried on, sending every
/// frame through composition instead.
pub fn lower_layer(input: &LoweringInput, dst: &mut PlaneLayer) {
    let display = &input.display;
    let format = input.format;
    let driver_owns_binding = input.binding == BindingModel::DriverOwnsBinding;

    dst.set_externally_bound(driver_owns_binding);
    if !driver_owns_binding {
        dst.set_property(PropTag::FbId, u64::from(input.fb_id));
    }

    dst.set_property(PropTag::CrtcId, u64::from(input.crtc_id));

    // Destination is whole signed pixels; source is 16.16 fixed point.
    dst.set_property(
        PropTag::CrtcX,
        u64::from(display.dst_rect.x.cast_unsigned()),
    );
    dst.set_property(
        PropTag::CrtcY,
        u64::from(display.dst_rect.y.cast_unsigned()),
    );
    dst.set_property(PropTag::CrtcW, u64::from(display.dst_rect.w));
    dst.set_property(PropTag::CrtcH, u64::from(display.dst_rect.h));

    let src_w = if display.src_rect.w == 0 {
        format.width
    } else {
        display.src_rect.w
    };
    let src_h = if display.src_rect.h == 0 {
        format.height
    } else {
        display.src_rect.h
    };
    dst.set_property(PropTag::SrcX, to_16_16(display.src_rect.x.cast_unsigned()));
    dst.set_property(PropTag::SrcY, to_16_16(display.src_rect.y.cast_unsigned()));
    dst.set_property(PropTag::SrcW, to_16_16(src_w));
    dst.set_property(PropTag::SrcH, to_16_16(src_h));

    // Format and modifier let the allocator screen planes statically before any
    // test commit. These are internal hints on the property bag, not KMS plane
    // property names.
    dst.set_property(PropTag::PixelFormat, u64::from(format.fourcc));
    dst.set_property(PropTag::FbModifier, format.modifier);

    if display.rotation != 0 {
        dst.set_property(PropTag::Rotation, display.rotation);
    }
    if let Some(zpos) = display.zpos.or(input.default_zpos) {
        dst.set_property(PropTag::Zpos, zpos);
    }
    if let Some(alpha) = display.alpha {
        dst.set_property(PropTag::Alpha, u64::from(alpha));
    }
}
