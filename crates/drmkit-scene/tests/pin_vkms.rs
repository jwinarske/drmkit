// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Pinning a layer to a chosen plane, against a real device.
//!
//! Parity port of `tests/integration/test_layer_scene_pin_vkms.cpp` from
//! drm-cxx @ `4a0b64a`.
//!
//! A pin is a *request*. The allocator skips a pinned layer entirely — the
//! scene owns its plane from then on — so an unhonourable pin has to be caught
//! before that happens, or the layer would be skipped by the allocator and
//! placed by nobody.

mod common;

use common::{LAYER_H, LAYER_W, card_guard, fixture, open_card};
use drmkit_scene::Placement;

/// A pinned layer lands on the plane it asked for.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_pinned_layer_lands_on_its_own_plane_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };

    // An overlay rather than the primary: pinning to the plane the allocator
    // would have chosen anyway proves nothing.
    let Some(target) = fx.an_overlay_plane() else {
        drmkit_testkit::skipped("no non-primary plane on this CRTC to pin to");
        return;
    };
    let Some(handle) = fx.add_layer(0, 0, LAYER_W, LAYER_H) else {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    };
    fx.scene
        .layer_mut(handle)
        .expect("the layer")
        .set_pinned_plane(Some(target));

    let report = fx.commit().expect("the pinned frame");

    assert_eq!(
        report.pin_requests_unhonored, 0,
        "the plane is on this CRTC and takes the format, so the pin is honourable"
    );
    let placement = report
        .placements
        .iter()
        .find(|entry| entry.layer == handle.layer_id())
        .expect("the pinned layer has to be reported like any other");
    assert_eq!(
        placement.placement,
        Placement::AssignedToPlane,
        "a pinned layer the allocator skips must still reach scanout"
    );
    assert_eq!(
        placement.plane_id,
        Some(target),
        "on the plane that was asked for, not whichever one was free"
    );
    assert!(
        report.accounting_balances(),
        "a pinned layer is in `considered`, so it has to be in exactly one \
         of the outcome tallies too"
    );

    // Reported assigned is not the same as programmed. The allocator skips a
    // pinned layer, so if nothing else puts it in the plan the report would
    // claim a plane the commit never wrote -- and the plane would be dark.
    assert!(
        report.fbs_attached >= 1,
        "the frame attached no framebuffer at all, so whatever the report \
         says, nothing reached a plane"
    );
    let fb = fx.plane_framebuffer(target);
    assert!(
        fb.is_some_and(|fb| fb != 0),
        "the pinned plane is scanning out nothing; the pin reached the report \
         but not the kernel"
    );

    fx.teardown();
}

/// A pin the hardware cannot honour falls back, and says so.
///
/// The alternative is worse than it sounds. A pin to a plane that cannot take
/// the layer would fail the `TEST_ONLY` and take the *whole frame* down —
/// every correctly placed layer with it. Refusing the pin costs the caller
/// determinism; honouring an impossible one costs everyone the frame.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn an_impossible_pin_falls_back_and_is_counted_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    let Some(handle) = fx.add_layer(0, 0, LAYER_W, LAYER_H) else {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    };

    // A plane id no device has. The other two ways a pin fails -- another
    // CRTC's plane, and a plane that cannot scan the format out -- need
    // hardware with more than one CRTC or a pickier plane than vkms has.
    fx.scene
        .layer_mut(handle)
        .expect("the layer")
        .set_pinned_plane(Some(0xDEAD_BEEF));

    let report = fx
        .commit()
        .expect("an impossible pin is not a failed frame");

    assert_eq!(
        report.pin_requests_unhonored, 1,
        "silence here would leave a caller believing its deterministic-plane \
         assumption held"
    );
    let placement = report
        .placements
        .iter()
        .find(|entry| entry.layer == handle.layer_id())
        .expect("the layer is still in the frame");
    assert_ne!(
        placement.placement,
        Placement::Unassigned,
        "a refused pin falls back to normal allocation; it does not drop the \
         layer"
    );
    assert!(report.accounting_balances());

    fx.teardown();
}
