// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Stacking on a CRTC whose planes have no `zpos`, read back from the screen.
//!
//! Port of the three cases drm-cxx `03bc1d7` adds to
//! `tests/integration/test_layer_scene_composition_vkms.cpp`. vkms exposes no
//! `zpos` property, so the kernel stacks its planes by id and only the plane a
//! layer lands on decides what covers what. Every assignment here is valid to
//! the kernel, which is why the report cannot tell a right frame from a wrong
//! one and the cases read the pixels back.

mod common;

use common::{Fixture, card_guard, fixture, open_card};
use drmkit_dumb::MapAccess;
use drmkit_scene::{LayerHandle, Placement};

/// Side of every layer: all of them are stacked on one spot at screen center.
const SIDE: u32 = 64;

/// A distinct opaque color per index.
fn color(index: u32) -> u32 {
    (((index * 15) & 0xFF) << 16) | 0x80
}

/// Add `count` layers at the screen center, zpos `index + 3`, each painted
/// [`color`]. `None` when a buffer cannot be had.
fn stack(fx: &mut Fixture, count: u32) -> Option<Vec<LayerHandle>> {
    let (w, h) = fx.mode_size();
    let (x, y) = (
        i32::try_from((w - SIDE) / 2).ok()?,
        i32::try_from((h - SIDE) / 2).ok()?,
    );
    let mut handles = Vec::new();
    for index in 0..count {
        let handle = fx.add_layer(x, y, SIDE, SIDE)?;
        paint(fx, handle, color(index))?;
        set_zpos(fx, handle, u64::from(index) + 3);
        handles.push(handle);
    }
    Some(handles)
}

fn paint(fx: &mut Fixture, handle: LayerHandle, xrgb: u32) -> Option<()> {
    let layer = fx.scene.layer_mut(handle)?;
    let mut mapping = layer.source_mut().map(MapAccess::Write).ok()?;
    let stride = mapping.stride() as usize;
    let width = mapping.width() as usize;
    for row in mapping.pixels_mut().chunks_mut(stride) {
        for pixel in row[..width * 4].chunks_exact_mut(4) {
            pixel.copy_from_slice(&(0xFF00_0000 | xrgb).to_le_bytes());
        }
    }
    Some(())
}

fn set_zpos(fx: &mut Fixture, handle: LayerHandle, zpos: u64) {
    let layer = fx.scene.layer_mut(handle).expect("the layer");
    let mut display = *layer.display();
    display.zpos = Some(zpos);
    layer.set_display(display);
}

/// The color at the screen center, alpha dropped.
fn center(fx: &Fixture) -> u32 {
    let image = drmkit_capture::snapshot(&fx.device, fx.crtc_id()).expect("snapshot");
    let (x, y) = (image.width() / 2, image.height() / 2);
    image.pixels()[(y * image.width() + x) as usize] & 0x00FF_FFFF
}

/// A fixture with a full-screen canvas.
fn composing_fixture() -> Option<Fixture> {
    let mut fx = fixture(open_card()?)?;
    let (w, h) = fx.mode_size();
    let device = &fx.device;
    let canvas = fx.scene.enable_composition(device, w, h);
    canvas.ok()?;
    Some(fx)
}

/// More layers than planes, all on one spot. The canvas carries the layers the
/// allocator dropped, so the topmost layer's color shows only if the canvas
/// plane sits above every plane carrying a layer below the ones it carries
/// (drm-cxx#240).
#[test]
#[ignore = "needs a DRM device"]
fn the_canvas_stacks_where_its_layers_asked_vkms() {
    let _guard = card_guard();
    let Some(mut fx) = composing_fixture() else {
        drmkit_testkit::skipped("no connected output or no canvas");
        return;
    };
    let count = u32::try_from(fx.eligible_planes() + 4).expect("few planes");
    let Some(_handles) = stack(&mut fx, count) else {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    };

    let report = fx.commit().expect("the frame");
    assert!(
        report.layers_composited > 0,
        "the stack should overflow into the canvas: {report:?}"
    );
    assert_eq!(report.layers_unassigned, 0, "{report:?}");
    assert_eq!(center(&fx), color(count - 1), "the topmost layer must show");
    fx.teardown();
}

/// Plane pressure with the low-priority layers mid-stack. Only they may go to
/// the canvas, and the stack must still render top-down: the composited run
/// is contiguous, so the canvas can sit between its neighbors.
#[test]
#[ignore = "needs a DRM device"]
fn the_composited_run_takes_the_low_priority_layers_vkms() {
    let _guard = card_guard();
    let Some(mut fx) = composing_fixture() else {
        drmkit_testkit::skipped("no connected output or no canvas");
        return;
    };
    let count = u32::try_from(fx.eligible_planes() + 4).expect("few planes");
    let Some(handles) = stack(&mut fx, count) else {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    };
    // The middle half is cheap to composite, the rest is not.
    let low = |index: u32| index >= count / 4 && index < count * 3 / 4;
    for (index, handle) in (0..count).zip(&handles) {
        fx.scene
            .layer_mut(*handle)
            .expect("the layer")
            .set_app_priority(if low(index) { 10 } else { 200 });
    }

    let report = fx.commit().expect("the frame");
    assert!(report.layers_composited > 0, "{report:?}");
    assert_eq!(report.layers_unassigned, 0, "{report:?}");
    for (index, handle) in (0..count).zip(&handles) {
        let placed = report
            .placements
            .iter()
            .find(|entry| entry.layer == handle.layer_id())
            .is_some_and(|entry| entry.placement == Placement::AssignedToPlane);
        assert!(
            placed || low(index),
            "layer {index} (high priority) was composited"
        );
    }
    assert_eq!(center(&fx), color(count - 1));
    fx.teardown();
}

/// Reversing the zpos of overlapping layers after steady frames must reach the
/// screen. The warm start's cached assignment is still valid to the kernel,
/// just stacked in the old order (drm-cxx#239).
#[test]
#[ignore = "needs a DRM device"]
fn a_restack_reaches_the_screen_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    let Some(handles) = stack(&mut fx, 3) else {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    };
    fx.commit().expect("the first frame");
    fx.commit().expect("a steady frame");
    assert_eq!(center(&fx), color(2), "before the restack");

    for (index, handle) in (0u64..).zip(&handles) {
        set_zpos(&mut fx, *handle, 5 - index);
    }
    fx.commit().expect("the restacked frame");
    assert_eq!(
        center(&fx),
        color(0),
        "the layer now on top must show after the restack"
    );
    fx.teardown();
}
