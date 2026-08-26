// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Picking a format the display controller will actually take.

use drm::control::Device as _;
use drmkit_core::Device;
use drmkit_planes::{PlaneRegistry, PlaneType};

/// The default preference order: most colour depth first, 16-bit last.
///
/// Packed formats only. A CPU producer that has just finished a frame has it
/// in one buffer, and handing it a planar format would mean it had to split
/// it — which is a decision for whoever knows the pixel pipeline, not for a
/// default.
pub const DEFAULT_PREFERENCE: [u32; 6] = [
    drmkit_fmt::fourcc::XRGB8888,
    drmkit_fmt::fourcc::XBGR8888,
    drmkit_fmt::fourcc::ARGB8888,
    drmkit_fmt::fourcc::ABGR8888,
    drmkit_fmt::fourcc::RGB565,
    drmkit_fmt::fourcc::BGR565,
];

/// The first format in `preferred` that a plane on `crtc_id` can scan out.
///
/// Pass an empty `preferred` for [`DEFAULT_PREFERENCE`]. A caller that can only
/// render certain formats should pass its own renderable subset, so the answer
/// is always something it can produce — asking for the best format the display
/// accepts is no use if the producer cannot draw it.
///
/// # Why this exists
///
/// Display controllers do not all advertise `XRGB8888`. TI's tilcdc on a
/// `BeagleBone` Black exposes `RGB565`, `XBGR8888` and `BGR888` and nothing
/// else, so a CPU producer that hardcodes `XRGB8888` is refused at commit — by
/// an `EINVAL` that names no format.
///
/// # What `None` means
///
/// No plane on that CRTC takes any of the preferred formats, or the planes
/// could not be queried at all. The reference returns `0` for both and for
/// "the CRTC does not exist"; those are different situations and none of them
/// is a format, so this returns `Option` and leaves the caller to decide what
/// to fall back to.
///
/// The primary plane is asked first, since a full-screen scanout lands there,
/// and any other compatible plane after — a driver that advertises a format on
/// an overlay and not the primary is unusual, but refusing to look is worse
/// than looking.
#[must_use]
pub fn negotiate_scanout_format(device: &Device, crtc_id: u32, preferred: &[u32]) -> Option<u32> {
    let registry = PlaneRegistry::probe(device).ok()?;
    let resources = device.resource_handles().ok()?;
    let index = resources
        .crtcs()
        .iter()
        .position(|handle| u32::from(*handle) == crtc_id)?;
    let index = u32::try_from(index).ok()?;

    let prefs = if preferred.is_empty() {
        &DEFAULT_PREFERENCE[..]
    } else {
        preferred
    };

    let pick = |plane: &drmkit_planes::PlaneCapabilities| {
        prefs
            .iter()
            .copied()
            .find(|fourcc| plane.supports_format(*fourcc))
    };

    if let Some(fourcc) = registry
        .for_crtc(index)
        .find(|plane| plane.plane_type == PlaneType::Primary)
        .and_then(pick)
    {
        return Some(fourcc);
    }
    registry.for_crtc(index).find_map(pick)
}
