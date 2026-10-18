// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A layer hanging off the top-left corner, against a live CRTC.
//!
//! `CRTC_X` and `CRTC_Y` are signed-range properties, and the kernel
//! range-checks the value as an `i64`, so a negative position has to be
//! sign-extended into the property's slot. Written as a 32-bit two's
//! complement it reads as about four billion and the commit is refused.

mod common;

use common::{card_guard, fixture, open_card};
use drmkit_scene::Placement;

/// A layer partly off the left and top edges gets a plane, and the kernel
/// holds the negative position it was given.
///
/// The placement is the assertion that matters. A refused position does not
/// fail the frame: the scene falls back to compositing the layer, which draws
/// it into the canvas and commits fine. So what shows the encoding is wrong is
/// a layer that should have had a plane and did not.
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

    assert_eq!(
        fx.plane_property(plane, "CRTC_X"),
        Some((-16_i64).cast_unsigned())
    );
    assert_eq!(
        fx.plane_property(plane, "CRTC_Y"),
        Some((-8_i64).cast_unsigned())
    );
    fx.teardown();
}
