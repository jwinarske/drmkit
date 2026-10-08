// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The composition canvas counts against the CRTC's plane limit.
//!
//! Port of `tests/integration/test_layer_scene_plane_limit_vkms.cpp` (drm-cxx
//! `893a938`, drm-cxx#244). Some controllers light fewer planes at once than
//! they advertise -- RK3566 VOP2 has three eligible and two usable -- and
//! refuse any commit that arms one more. The canvas is the plane that tips
//! such a frame over: the layer assignment passes its test, then the canvas
//! joins it. vkms has no such limit, so the rig imposes one on every test and
//! on the real commit, the way upstream interposes `drmModeAtomicCommit`.

mod common;

use common::{card_guard, fixture, open_card};

/// Four layers on a CRTC that lights two planes. The frame must commit, every
/// layer must reach the screen -- the canvas carries what the planes cannot --
/// and the settled frame must hold at one warm-start test.
#[test]
#[ignore = "needs a DRM device"]
fn the_canvas_counts_against_the_plane_limit_vkms() {
    const SIDE: u32 = 64;
    const LIMIT: usize = 2;
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    if fx.enable_full_screen_composition().is_none() {
        drmkit_testkit::skipped("no canvas");
        return;
    }
    if fx.eligible_planes() <= LIMIT {
        drmkit_testkit::skipped(&format!(
            "{} plane(s) on this CRTC: a limit of {LIMIT} cannot bite",
            fx.eligible_planes()
        ));
        fx.teardown();
        return;
    }
    let (w, h) = fx.mode_size();

    // A background and three disjoint tiles, each its own color.
    let colors = [0x0020_2020, 0x00FF_0000, 0x0000_FF00, 0x0000_00FF];
    let mut probes = Vec::new();
    for (index, color) in (0u32..).zip(colors) {
        let (x, y, lw, lh) = if index == 0 {
            (0, 0, w, h)
        } else {
            (index * 2 * SIDE, SIDE, SIDE, SIDE)
        };
        let Some(handle) = fx.add_layer(
            i32::try_from(x).expect("on screen"),
            i32::try_from(y).expect("on screen"),
            lw,
            lh,
        ) else {
            drmkit_testkit::skipped("no dumb buffer for a layer");
            return;
        };
        fx.paint(handle, color).expect("paint");
        fx.set_zpos(handle, u64::from(index) + 3);
        probes.push(if index == 0 {
            (w - 1, h - 1)
        } else {
            (x + SIDE / 2, y + SIDE / 2)
        });
    }

    let (first, refused) = fx
        .commit_limited(LIMIT)
        .expect("the frame armed more planes than the CRTC lights");
    let (second, _) = fx.commit_limited(LIMIT).expect("the settled frame");

    assert_eq!(first.layers_unassigned, 0, "{first:?}");
    assert!(first.layers_composited >= 2, "{first:?}");
    assert!(refused > 0, "the limit never bit: the case proves nothing");
    assert!(
        second.test_commits_issued <= 1,
        "the settled frame should hold at one warm-start test: {second:?}"
    );
    for (index, ((x, y), color)) in probes.into_iter().zip(colors).enumerate() {
        let Some(pixel) = fx.pixel_at(x, y) else {
            println!("note: the CRTC cannot be read back; pixels unchecked");
            break;
        };
        assert_eq!(
            pixel, color,
            "layer {index} is missing from the committed frame"
        );
    }
    fx.teardown();
}
