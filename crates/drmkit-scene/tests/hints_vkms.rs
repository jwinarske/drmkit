// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Allocator hints — content type, refresh rate, priority — against a real
//! device.
//!
//! Parity port of `tests/integration/test_layer_scene_content_type_vkms.cpp`
//! and `test_layer_scene_app_priority_vkms.cpp` from drm-cxx @ `4a0b64a`.
//!
//! Hints change plane **scoring**, which is why they drop the warm start where
//! geometry does not. These pin that the drop actually happens against real
//! planes, and that the flag is cleared once the allocation has seen it — a
//! hint that stayed dirty would re-solve the scene every frame thereafter.

mod common;

use common::{LAYER_H, LAYER_W, card_guard, fixture, open_card};
use drmkit_scene::Placement;

/// A content-type change drops the warm start and re-validates the placement.
///
/// Content type feeds plane scoring: a layer that becomes video may prefer a
/// plane with the YUV pipeline or the bandwidth for it. A scene that kept the
/// warm start would leave it where it was and never offer it the plane the
/// hint exists to reach.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_content_type_change_forces_a_reallocation_vkms() {
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

    fx.commit().expect("the cold frame");
    let steady = fx.commit().expect("the steady frame");
    assert_eq!(
        steady.test_commits_issued, 0,
        "a steady frame asks the kernel nothing new"
    );
    assert!(steady.fb_delta_fast_path);

    let layer = fx.scene.layer_mut(handle).expect("the layer");
    assert!(
        !layer.hints_dirty(),
        "the previous commit saw the hints, so nothing is outstanding"
    );
    layer.set_content_type(drmkit_planes::ContentType::Video);
    assert!(
        layer.hints_dirty(),
        "a scoring change the allocation has not seen yet"
    );
    assert_eq!(layer.content_type(), drmkit_planes::ContentType::Video);

    let promoted = fx.commit().expect("the frame after the hint change");
    assert!(
        !promoted.fb_delta_fast_path,
        "a scoring change must drop the warm start, or the layer never gets \
         offered the plane the hint is for"
    );
    assert!(
        fx.scene
            .layer(handle)
            .is_some_and(|layer| !layer.hints_dirty()),
        "the commit has to clear the flag, or every frame after this one \
         re-solves the whole scene"
    );
    assert!(
        promoted
            .placements
            .iter()
            .any(|p| p.placement != Placement::Unassigned),
        "the layer still has to reach the screen"
    );

    let after = fx.commit().expect("the frame after that");
    assert!(
        after.fb_delta_fast_path,
        "with the hint seen, the warm start comes back"
    );

    fx.teardown();
}

/// A refresh-rate hint round-trips and is cleared by the commit.
///
/// The hint is advisory — it tells the allocator how often this layer expects
/// to change — but the dirty flag is not: a hint that stayed dirty would cost
/// a full search every frame for the rest of the scene's life.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_refresh_hint_round_trips_through_a_commit_vkms() {
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

    fx.commit().expect("the cold frame");

    let layer = fx.scene.layer_mut(handle).expect("the layer");
    assert_eq!(layer.update_hint_hz(), 0, "no hint is the default");
    layer.set_update_hint(30);
    assert!(layer.hints_dirty());
    assert_eq!(
        layer.update_hint_hz(),
        30,
        "what was set is what is read back"
    );

    fx.commit().expect("the frame after the hint");
    assert!(
        fx.scene
            .layer(handle)
            .is_some_and(|layer| !layer.hints_dirty()),
        "the allocation has seen it"
    );
    assert_eq!(
        fx.scene
            .layer(handle)
            .map(drmkit_scene::SceneLayer::update_hint_hz),
        Some(30),
        "clearing the flag must not clear the hint itself"
    );

    fx.teardown();
}

/// Under plane pressure, the layers that keep planes outrank those that lose
/// them.
///
/// More layers than planes is the case priority exists for. Which specific
/// layer lands where is the allocator's business and varies with the hardware;
/// what must hold on any of it is the ordering — every layer on a plane
/// outranks every layer that was not, or the caller's priority meant nothing.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn under_plane_pressure_the_placed_layers_outrank_the_dropped_ones_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };

    // Enough layers to exceed what this CRTC can scan out, measured rather
    // than assumed: vkms advertises far more planes than a typical SoC, and a
    // fixed count that applies pressure on vc4 applies none here.
    let planes = fx.eligible_planes();
    let count = planes + 2;
    assert!(
        count >= 3,
        "a CRTC with fewer than one usable plane is not something to test \
         placement priority on"
    );

    // Priorities spread across the range and deliberately not in the order the
    // layers are added, so an allocator that simply kept the first few would
    // fail the ordering rather than pass by accident.
    let priorities: Vec<u8> = (0..count)
        .map(|index| {
            let step = u8::try_from(255 / count.max(1)).unwrap_or(1);
            // A shuffle with a fixed stride: consecutive layers are far apart
            // in priority, and the highest is not the first or the last.
            step.wrapping_mul(u8::try_from((index * 7 + 3) % count).unwrap_or(0))
        })
        .collect();
    let mut layers = Vec::new();
    for (index, priority) in priorities.iter().copied().enumerate() {
        let offset = i32::try_from(index).expect("a small index") * 8;
        let Some(handle) = fx.add_layer(offset, offset, 32, 32) else {
            drmkit_testkit::skipped("no dumb buffer for a layer");
            return;
        };
        fx.scene
            .layer_mut(handle)
            .expect("the layer")
            .set_app_priority(priority);
        layers.push((handle, priority));
    }

    let report = fx.commit().expect("the frame under pressure");
    assert_eq!(report.layers_total, priorities.len());
    println!(
        "note: {} eligible planes, {} layers",
        planes,
        priorities.len()
    );

    let mut worst_placed = u16::from(u8::MAX) + 1;
    let mut best_dropped: i32 = -1;
    for (handle, priority) in &layers {
        let placed = report
            .placements
            .iter()
            .find(|entry| entry.layer == handle.layer_id())
            .is_some_and(|entry| entry.placement == Placement::AssignedToPlane);
        if placed {
            worst_placed = worst_placed.min(u16::from(*priority));
        } else {
            best_dropped = best_dropped.max(i32::from(*priority));
        }
    }

    if best_dropped < 0 {
        println!("note: every layer reached a plane; this CRTC has no pressure to apply");
    } else {
        assert!(
            i32::from(worst_placed) > best_dropped,
            "a layer at priority {best_dropped} lost its plane to one at \
             {worst_placed} or lower -- the caller's ordering was not honoured"
        );
    }

    fx.teardown();
}
