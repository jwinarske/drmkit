// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! `DisplayParams::src_rect_fixed` against a live CRTC: a 16.16 source
//! rectangle reaches the plane's `SRC_*` as given, so a sub-pixel crop is not
//! rounded to whole pixels on the way.
//!
//! Parity port of `tests/integration/test_layer_scene_src_fixed_vkms.cpp`
//! from drm-cxx @ `30c4e3f`.

mod common;

use common::{card_guard, fixture, open_card};
use drmkit_scene::{DisplayParams, FixedRect, Rect};

/// `LayerSceneSrcFixedVkms.SubPixelCropReachesTheKernelUnrounded`: a crop
/// that starts half a pixel in, the size of the destination. No scaling, so
/// any plane can take it, and `SRC_X` must read back as 0.5 rather than 0.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_sub_pixel_crop_reaches_the_kernel_unrounded_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    let (w, h) = fx.mode_size();
    // One column wider than the crop, so the half-pixel offset stays inside.
    let Some(handle) = fx.add_layer(0, 0, w + 1, h) else {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    };
    fx.scene
        .layer_mut(handle)
        .expect("the layer")
        .set_display(DisplayParams {
            src_rect_fixed: Some(FixedRect {
                x: 0x8000,
                y: 0,
                w: w << 16,
                h: h << 16,
            }),
            dst_rect: Rect { x: 0, y: 0, w, h },
            ..DisplayParams::default()
        });

    let report = fx.commit().expect("the frame");
    let plane = report
        .placements
        .iter()
        .find_map(|p| p.plane_id)
        .expect("the crop is unscaled, so a plane takes it");

    assert_eq!(fx.plane_property(plane, "SRC_X"), Some(0x8000));
    assert_eq!(fx.plane_property(plane, "SRC_Y"), Some(0));
    assert_eq!(fx.plane_property(plane, "SRC_W"), Some(u64::from(w) << 16));
    assert_eq!(fx.plane_property(plane, "SRC_H"), Some(u64::from(h) << 16));
    fx.teardown();
}
