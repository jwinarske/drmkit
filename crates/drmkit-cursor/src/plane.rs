// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Choosing the plane a cursor is drawn on.
//!
//! This decides everything downstream: whether the cursor is committed
//! atomically with the rest of the frame, whether it can be rotated in
//! hardware, and how big its buffer may be. It is a policy over what the
//! registry says, with no device in it -- the one thing that genuinely needs
//! to ask the kernel, whether a plane is already bound elsewhere, is a
//! predicate the caller supplies.
//!
//! The reference enumerates the registry and calls `drmModeGetPlane` itself,
//! which makes the policy untestable without hardware. Splitting the question
//! out is what lets every case below be a test.

use drmkit_planes::{PlaneCapabilities, PlaneRegistry, PlaneType};

use crate::CursorError;

/// How the cursor reaches the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanePath {
    /// A dedicated cursor plane, driven atomically.
    ///
    /// The best case: per-CRTC hardware with no z-ordering to negotiate.
    AtomicCursor,
    /// An overlay plane, driven atomically.
    ///
    /// For hardware with no cursor plane, or whose cursor plane cannot take
    /// `ARGB8888`. Costs an overlay the compositor might have wanted.
    AtomicOverlay,
    /// `drmModeSetCursor`, the legacy path.
    ///
    /// No atomic commit, so the cursor cannot be coalesced with a frame and
    /// cannot be rotated. Last resort.
    Legacy,
}

/// The plane a cursor will use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedPlane {
    /// The plane's KMS id, or `0` on the legacy path, which addresses the CRTC
    /// rather than a plane.
    pub plane_id: u32,
    /// How it will be driven.
    pub path: PlanePath,
    /// The plane's maximum cursor width, when it advertises one.
    pub cursor_max_w: u32,
    /// The plane's maximum cursor height, when it advertises one.
    pub cursor_max_h: u32,
    /// The `FourCC` the cursor buffer is allocated and drawn in.
    ///
    /// Not always `ARGB8888`. Tegra's cursor plane advertises `RGBA8888` and
    /// nothing else, and a plane that differs only in channel order is
    /// perfectly able to carry a cursor -- see [`CURSOR_FORMATS`].
    pub fourcc: u32,
}

/// The formats a cursor can be drawn in, best first.
///
/// All four are 32-bit with alpha and differ only in channel order, so the
/// renderer reaches any of them by permuting the pixel it already has. There
/// is no `XRGB8888` here on purpose: a cursor without an alpha channel is a
/// rectangle.
///
/// `ARGB8888` leads because it is what the shadow is already in and what the
/// legacy path requires; the rest are taken only when a plane offers nothing
/// better.
///
/// This list is why the port lights a cursor plane on hardware the reference
/// falls back from. `plane_supports_argb8888` there tests one `FourCC` for
/// equality, so Tegra's cursor plane -- `RGBA8888` only -- is rejected and an
/// overlay is spent on a cursor the dedicated hardware could have carried.
pub const CURSOR_FORMATS: [u32; 4] = [
    drmkit_fmt::fourcc::ARGB8888,
    drmkit_fmt::fourcc::RGBA8888,
    drmkit_fmt::fourcc::ABGR8888,
    drmkit_fmt::fourcc::BGRA8888,
];

/// The format this plane would carry a cursor in, if it can carry one.
///
/// [`CURSOR_FORMATS`] in order, so a plane that takes `ARGB8888` is drawn in
/// what the shadow already holds and nothing is permuted.
fn usable(plane: &PlaneCapabilities) -> Option<u32> {
    CURSOR_FORMATS
        .into_iter()
        .find(|fourcc| plane.supports_format(*fourcc))
}

/// Pick a plane for the cursor on `crtc_index`.
///
/// `forced` bypasses the search: the named plane is validated and taken, or
/// the call fails. That is a caller overriding policy, and silently ignoring
/// an override is worse than refusing it.
///
/// `bound_elsewhere` answers whether a plane is currently driving some other
/// CRTC. Overlays are shared on many parts, and taking one that another output
/// is using costs that output a layer; the search prefers a free one but will
/// take a busy one rather than fall back to legacy.
///
/// # Errors
///
/// - [`CursorError::NoPlane`] when nothing on this CRTC can carry a cursor and
///   `allow_legacy` is false.
/// - [`CursorError::PlaneUnusable`] when `forced` names a plane that does not
///   exist, cannot feed this CRTC, or cannot take `ARGB8888`.
pub fn select_plane(
    registry: &PlaneRegistry,
    crtc_index: u32,
    forced: Option<u32>,
    allow_legacy: bool,
    bound_elsewhere: &dyn Fn(u32) -> bool,
) -> Result<SelectedPlane, CursorError> {
    if let Some(plane_id) = forced {
        let plane = registry
            .by_id(plane_id)
            .filter(|plane| plane.compatible_with_crtc(crtc_index))
            .ok_or(CursorError::PlaneUnusable)?;
        let fourcc = usable(plane).ok_or(CursorError::PlaneUnusable)?;
        return Ok(selected(plane, fourcc));
    }

    // A cursor plane first. It is per-CRTC hardware built for this, with no
    // z-order to negotiate and no overlay taken away from anyone.
    if let Some((plane, fourcc)) = registry
        .for_crtc(crtc_index)
        .filter(|plane| plane.plane_type == PlaneType::Cursor)
        .find_map(|plane| usable(plane).map(|fourcc| (plane, fourcc)))
    {
        return Ok(selected(plane, fourcc));
    }

    // Otherwise an overlay, preferring one no other CRTC is using. Some parts
    // expose only a handful, and a compositor wants the rest for layers.
    let mut fallback: Option<(&PlaneCapabilities, u32)> = None;
    for plane in registry.for_crtc(crtc_index) {
        if plane.plane_type != PlaneType::Overlay {
            continue;
        }
        let Some(fourcc) = usable(plane) else {
            continue;
        };
        if !bound_elsewhere(plane.id) {
            return Ok(selected(plane, fourcc));
        }
        fallback.get_or_insert((plane, fourcc));
    }
    if let Some((plane, fourcc)) = fallback {
        return Ok(selected(plane, fourcc));
    }

    if allow_legacy {
        return Ok(SelectedPlane {
            plane_id: 0,
            path: PlanePath::Legacy,
            cursor_max_w: 0,
            cursor_max_h: 0,
            // `drmModeSetCursor` has no format argument: the kernel reads the
            // buffer as ARGB8888 and there is nothing to negotiate.
            fourcc: drmkit_fmt::fourcc::ARGB8888,
        });
    }
    Err(CursorError::NoPlane)
}

/// Describe a chosen plane.
fn selected(plane: &PlaneCapabilities, fourcc: u32) -> SelectedPlane {
    let path = if plane.plane_type == PlaneType::Cursor {
        PlanePath::AtomicCursor
    } else {
        PlanePath::AtomicOverlay
    };
    SelectedPlane {
        plane_id: plane.id,
        path,
        // Only a cursor plane advertises a maximum; an overlay is bounded by
        // the mode, and reporting a zero here says "no limit of its own"
        // rather than "zero by zero".
        cursor_max_w: plane.cursor_max_w,
        cursor_max_h: plane.cursor_max_h,
        fourcc,
    }
}
