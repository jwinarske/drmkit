// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A layer hanging off the top-left corner, against a live CRTC.
//!
//! Some drivers -- the SA8155P's -- refuse a destination that starts above or
//! left of the CRTC, so the scene clips it to the CRTC and trims the source
//! to match, as the kernel's own clip would (drm-cxx#384).

mod common;

use common::{card_guard, fixture, open_card};
use drmkit_scene::Placement;

/// A layer partly off the left and top edges gets a plane, clipped to the
/// CRTC with its source trimmed to the part on screen.
///
/// The placement matters as much as the geometry. A refused position does not
/// fail the frame: the scene falls back to compositing the layer, which draws
/// it into the canvas and commits fine. So what shows the plane state is wrong
/// is a layer that should have had a plane and did not.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_layer_off_the_top_left_corner_gets_a_plane_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    let (w, h) = fx.mode_size();
    if fx.add_layer(0, 0, w, h).is_none() {
        drmkit_testkit::skipped("no layer buffer this device will frame");
        return;
    }
    let Some(corner) = fx.add_layer(-16, -8, 64, 64) else {
        drmkit_testkit::skipped("no layer buffer this device will frame");
        return;
    };

    let report = fx.commit().expect("the frame");
    let entry = report
        .placements
        .iter()
        .find(|entry| entry.layer == corner.layer_id())
        .expect("the corner layer is reported");
    assert_eq!(
        entry.placement,
        Placement::AssignedToPlane,
        "the corner layer was refused a plane"
    );
    let plane = entry.plane_id.expect("an assigned layer names its plane");

    let geometry = [
        "CRTC_X", "CRTC_Y", "CRTC_W", "CRTC_H", "SRC_X", "SRC_Y", "SRC_W", "SRC_H",
    ]
    .map(|name| fx.plane_property(plane, name));
    assert_eq!(
        geometry,
        [0, 0, 48, 56, 16 << 16, 8 << 16, 48 << 16, 56 << 16].map(Some),
        "the corner layer was not clipped to the CRTC"
    );
    fx.teardown();
}
