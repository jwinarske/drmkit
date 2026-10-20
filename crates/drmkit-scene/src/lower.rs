// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Lowering a scene layer into the plane property bag the allocator reads.

use drmkit_planes::{Layer as PlaneLayer, PropTag};

use crate::display::{DisplayParams, to_16_16};
use crate::source::{BindingModel, SourceFormat};

/// Everything `lower_layer` needs about a layer, without borrowing the scene.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers, with no default to fill a new field from"
)]
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
    /// The scene itself passes `None`; with this and the layer's own `zpos`
    /// both unset, `zpos` is not written.
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
///
/// One departure from the reference, drm-cxx#384: a destination starting above
/// or left of the CRTC is clipped to it, with the source trimmed to match. The
/// kernel would clip it the same way, but some drivers refuse a negative
/// position before it can.
pub fn lower_layer(input: &LoweringInput, dst: &mut PlaneLayer) {
    let display = &input.display;
    let format = input.format;
    let driver_owns_binding = input.binding == BindingModel::DriverOwnsBinding;

    dst.set_externally_bound(driver_owns_binding);
    if !driver_owns_binding {
        dst.set_property(PropTag::FbId, u64::from(input.fb_id));
    }

    dst.set_property(PropTag::CrtcId, u64::from(input.crtc_id));

    let mut source = if let Some(fixed) = display.src_rect_fixed {
        // Already 16.16: written as given, so a sub-pixel crop survives.
        let extent = |value: u32, full: u32| {
            if value == 0 {
                to_16_16(full)
            } else {
                u64::from(value)
            }
        };
        Source {
            x: u64::from(fixed.x),
            y: u64::from(fixed.y),
            w: extent(fixed.w, format.width),
            h: extent(fixed.h, format.height),
        }
    } else {
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
        Source {
            x: to_16_16(display.src_rect.x.cast_unsigned()),
            y: to_16_16(display.src_rect.y.cast_unsigned()),
            w: to_16_16(src_w),
            h: to_16_16(src_h),
        }
    };

    let mut dst_x = display.dst_rect.x;
    let mut dst_y = display.dst_rect.y;
    let mut dst_w = display.dst_rect.w;
    let mut dst_h = display.dst_rect.h;
    if clips_cleanly(display.rotation, format.fourcc) {
        (dst_x, dst_w, source.x, source.w) = clip_leading(dst_x, dst_w, source.x, source.w);
        (dst_y, dst_h, source.y, source.h) = clip_leading(dst_y, dst_h, source.y, source.h);
    }

    // Destination is whole signed pixels; source is 16.16 fixed point. The
    // position is sign-extended, as the reference's `static_cast` does: the
    // kernel range-checks a signed property as an `i64`, so a 32-bit two's
    // complement would read as a large positive number and be refused.
    dst.set_property(PropTag::CrtcX, i64::from(dst_x).cast_unsigned());
    dst.set_property(PropTag::CrtcY, i64::from(dst_y).cast_unsigned());
    dst.set_property(PropTag::CrtcW, u64::from(dst_w));
    dst.set_property(PropTag::CrtcH, u64::from(dst_h));
    dst.set_property(PropTag::SrcX, source.x);
    dst.set_property(PropTag::SrcY, source.y);
    dst.set_property(PropTag::SrcW, source.w);
    dst.set_property(PropTag::SrcH, source.h);

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

/// A source rectangle in 16.16 fixed point.
#[derive(Debug, Clone, Copy)]
struct Source {
    x: u64,
    y: u64,
    w: u64,
    h: u64,
}

/// Whether a destination starting off-screen can be clipped here.
///
/// Only unrotated: under a rotation or reflection the edge of the source to
/// trim is a different one, and getting that wrong shows the wrong part of the
/// buffer. Only formats whose every pixel stands alone: trimming a chroma-
/// subsampled buffer by an odd number of pixels starts it between two chroma
/// samples, which a driver may refuse or misdraw. Those layers keep their
/// off-screen position, as before.
const fn clips_cleanly(rotation: u64, fourcc: u32) -> bool {
    const ROTATE_0: u64 = 1;
    let unrotated = rotation == 0 || rotation == ROTATE_0;
    let packed = drmkit_fmt::format_bpp(fourcc) != 0;
    let subsampled = matches!(
        fourcc,
        drmkit_fmt::fourcc::YUYV
            | drmkit_fmt::fourcc::YVYU
            | drmkit_fmt::fourcc::UYVY
            | drmkit_fmt::fourcc::VYUY
            | drmkit_fmt::fourcc::Y210
            | drmkit_fmt::fourcc::Y212
            | drmkit_fmt::fourcc::Y216
    );
    unrotated && packed && !subsampled
}

/// Clip one axis of a destination that starts before the CRTC's origin, and
/// trim the source to match (drm-cxx#384).
///
/// The core KMS check clips such a plane itself, but some drivers -- the
/// SA8155P's -- refuse a negative destination before it gets the chance. The
/// trim follows the kernel's `drm_rect_clip_scaled`, so on a driver that does
/// clip, the plane shows exactly what it would have: the new source extent
/// rounds up when the plane is upscaled and down otherwise, and the source
/// keeps its far edge.
///
/// A destination entirely off-screen, or with no extent, is left alone: there
/// is nothing of it to show, and whatever the driver does with that is what it
/// did before.
fn clip_leading(dst_pos: i32, dst_len: u32, src_pos: u64, src_len: u64) -> (i32, u32, u64, u64) {
    let Ok(clip) = u32::try_from(-i64::from(dst_pos)) else {
        return (dst_pos, dst_len, src_pos, src_len);
    };
    if clip == 0 || clip >= dst_len {
        return (dst_pos, dst_len, src_pos, src_len);
    }
    let kept = dst_len - clip;
    let product = src_len * u64::from(kept);
    let dst_len_wide = u64::from(dst_len);
    let new_src_len = if src_len < dst_len_wide << 16 {
        product.div_ceil(dst_len_wide)
    } else {
        product / dst_len_wide
    };
    (0, kept, src_pos + (src_len - new_src_len), new_src_len)
}
