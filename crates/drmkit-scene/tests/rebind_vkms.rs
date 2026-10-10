// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Moving a scene to another output, against a real device.
//!
//! Parity port of `tests/integration/test_layer_scene_rebind_vkms.cpp` from
//! drm-cxx @ `4a0b64a`.

mod common;

use common::{LAYER_H, LAYER_W, card_guard, fixture, open_card};
use drmkit_scene::IncompatibilityReason;

/// A rebind to the same output keeps every layer and re-emits everything.
///
/// The no-op case is the one that catches a rebind which throws too much away.
/// Handles have to survive — a caller forced to rebuild its layers would have
/// to rebuild everything it knows about them — and the first commit afterwards
/// has to be a full emit, because the cached baseline described planes on an
/// output the scene no longer claims to be on.
///
/// **What this cannot separate on one CRTC.** Downgrading `forget_output` to
/// `invalidate_allocation`, which keeps the committed baseline, still passes:
/// a rebind also drops the warm start and re-dirties every layer, and on a
/// nine-plane vkms CRTC the fresh search lands the layer somewhere with no
/// baseline anyway. Verified by injection. Telling the two apart needs a
/// rebind to a *different* CRTC, which needs a second connected output.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_no_op_rebind_keeps_the_layers_and_re_emits_everything_vkms() {
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
        steady.properties_written, 1,
        "steady state writes only FB_ID, which is the state a rebind has to \
         undo"
    );

    let (width, height) = fx.mode_size();
    let report = fx.scene.rebind(fx.crtc_id(), width, height);
    assert!(
        report.is_empty(),
        "nothing changed, so nothing can have stopped fitting"
    );
    assert!(
        fx.scene.layer(handle).is_some(),
        "the handle has to survive, or the caller's own state is orphaned"
    );
    assert!(
        fx.scene.pending_detach().is_none(),
        "a rebind to the same CRTC has nothing to turn off"
    );

    // Deliberately *not* re-stating the mode: this is a rebind to the same
    // CRTC, which is already lit, so the only thing that can inflate the write
    // count is the cleared baseline. Forcing a modeset here would add its own
    // three properties and the assertion would hold whether or not the
    // baseline was dropped -- which is exactly what the first version of this
    // case did, and it passed against a rebind that kept the baseline.
    let after = fx.commit().expect("the frame after the rebind");
    assert!(
        after.properties_written > 1,
        "wrote {} properties; a rebind that kept the old output's baseline \
         would diff this frame against planes the new pipe was never told \
         about and emit the steady-state one",
        after.properties_written
    );
    assert!(
        after.layers_assigned >= 1,
        "and the layer still has to reach a plane"
    );

    fx.teardown();
}

/// An identity tag round-trips and survives a rebind.
///
/// A handle identifies a layer within one scene's lifetime. A tag identifies
/// what the layer *is* to the caller, which is what a caller needs after an
/// output changed underneath it.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn an_identity_tag_round_trips_and_survives_a_rebind_vkms() {
    const TAG_A: u64 = 0xA11CE;
    const TAG_B: u64 = 0xB0B;

    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    let (Some(a), Some(b)) = (
        fx.add_layer(0, 0, LAYER_W, LAYER_H),
        fx.add_layer(0, 0, LAYER_W, LAYER_H),
    ) else {
        drmkit_testkit::skipped("no dumb buffers for two layers");
        return;
    };

    fx.scene
        .layer_mut(a)
        .expect("layer a")
        .set_identity_tag(Some(TAG_A));
    fx.scene
        .layer_mut(b)
        .expect("layer b")
        .set_identity_tag(Some(TAG_B));

    assert_eq!(fx.scene.find_by_identity_tag(TAG_A), Some(a));
    assert_eq!(
        fx.scene.find_by_identity_tag(TAG_B),
        Some(b),
        "two tags must not resolve to the same layer"
    );
    assert_eq!(
        fx.scene.find_by_identity_tag(0xDEAD),
        None,
        "a tag nobody set resolves to nothing rather than to whoever is first"
    );

    let (width, height) = fx.mode_size();
    fx.scene.rebind(fx.crtc_id(), width, height);

    assert_eq!(
        fx.scene.find_by_identity_tag(TAG_A),
        Some(a),
        "the tag outlives the rebind, which is the whole reason to set one"
    );
    assert_eq!(fx.scene.find_by_identity_tag(TAG_B), Some(b));

    fx.teardown();
}

/// A layer that no longer fits the output is reported, not dropped.
///
/// The caller may be about to move it. Clamping it silently would put a window
/// somewhere the caller never asked for and cannot detect; dropping it would
/// lose content the caller still owns.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_layer_outside_the_new_mode_is_reported_not_dropped_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    let (width, height) = fx.mode_size();

    let Some(on_screen) = fx.add_layer(0, 0, LAYER_W, LAYER_H) else {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    };
    // Positioned past the right edge of the mode it is about to be rebound to.
    let Some(off_screen) = fx.add_layer(width.cast_signed(), 0, LAYER_W, LAYER_H) else {
        drmkit_testkit::skipped("no dumb buffer for a second layer");
        return;
    };

    let report = fx.scene.rebind(fx.crtc_id(), width, height);

    assert_eq!(
        report.incompatibilities.len(),
        1,
        "exactly the layer that does not fit -- flagging the one that does \
         would have a caller moving a window that was already correct"
    );
    assert_eq!(report.incompatibilities[0].handle, off_screen);
    assert_eq!(
        report.incompatibilities[0].reason,
        IncompatibilityReason::DstRectOffScreen
    );
    assert!(
        fx.scene.layer(off_screen).is_some(),
        "reported, not dropped: the caller still owns it and may be about to \
         move it"
    );
    assert!(fx.scene.layer(on_screen).is_some());

    fx.teardown();
}

/// A rebind to another CRTC hands a shared plane over, and leaves nothing lit.
///
/// The case a single CRTC cannot reach, and the one P-37 had backwards. The
/// layer is pinned to a plane both pipes can use. The kernel will not move a
/// plane between CRTCs in one commit ("switching CRTC directly"), so the
/// rebind has to queue a detach, and the first frame on the new CRTC must
/// wait for it -- without it that frame is rejected with `EINVAL`, and a plane
/// the new pipe cannot use goes on showing the old frame. Everything asserted
/// after the rebind is read back from the kernel.
///
/// Needs two connected outputs on different CRTCs and a plane both can use:
/// `validation/vkms-two-crtc.sh` builds one. `DRMKIT_TEST_CONNECTORS` names
/// the outputs on hardware, first the one to start on.
#[test]
#[ignore = "needs a DRM device, DRM master and two connected outputs"]
fn a_rebind_to_another_crtc_hands_a_shared_plane_over_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    let Some(other) = fx.another_output() else {
        drmkit_testkit::skipped("no second connected output on another CRTC");
        return;
    };
    let Some(shared) = fx.a_plane_shared_with(&other) else {
        drmkit_testkit::skipped("no plane both CRTCs can use");
        return;
    };
    let Some(handle) = fx.add_layer(0, 0, LAYER_W, LAYER_H) else {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    };
    fx.scene
        .layer_mut(handle)
        .expect("the layer")
        .set_pinned_plane(Some(shared));

    let old_crtc = fx.crtc_id();
    fx.commit().expect("the cold frame on the first output");
    fx.commit().expect("the steady frame");
    assert_eq!(
        fx.plane_property(shared, "CRTC_ID"),
        Some(u64::from(old_crtc)),
        "before the rebind the shared plane is on the first CRTC"
    );

    fx.switch_to(&other)
        .expect("learn the second output's planes");
    let (width, height) = fx.mode_size();
    let report = fx.scene.rebind(fx.crtc_id(), width, height);
    assert!(
        report.is_empty(),
        "a 64x64 layer at the origin fits any mode"
    );

    let detach = fx
        .scene
        .pending_detach()
        .expect("moving off a CRTC with lit planes has to queue their detach")
        .clone();
    assert_eq!(detach.crtc_id, old_crtc, "the detach is for the CRTC left");
    assert!(
        detach.planes.contains(&shared),
        "the shared plane is lit on the old CRTC, so it has to be in the detach"
    );
    let refused = fx.commit();
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("still lit")),
        "a frame built before the detach would be rejected by the kernel, so \
         the scene has to refuse it first; got {refused:?}"
    );

    assert_eq!(fx.commit_detach(), Ok(true), "the detach commits");
    assert!(fx.scene.pending_detach().is_none());
    for plane_id in &detach.planes {
        assert_eq!(
            fx.plane_property(*plane_id, "CRTC_ID"),
            Some(0),
            "plane {plane_id} is still on the old CRTC after the detach"
        );
    }

    let after = fx.commit().expect("the first frame on the second output");
    assert!(
        after.layers_assigned >= 1,
        "the pinned layer has to be placed"
    );
    assert_eq!(
        fx.plane_property(shared, "CRTC_ID"),
        Some(u64::from(fx.crtc_id())),
        "the plane did not follow the scene to the new CRTC"
    );
    assert!(
        fx.plane_framebuffer(shared).is_some_and(|fb| fb != 0),
        "the plane is on the new CRTC with nothing to scan out"
    );
    for (name, want) in [
        ("CRTC_X", 0),
        ("CRTC_Y", 0),
        ("CRTC_W", u64::from(LAYER_W)),
        ("CRTC_H", u64::from(LAYER_H)),
        ("SRC_W", u64::from(LAYER_W) << 16),
        ("SRC_H", u64::from(LAYER_H) << 16),
    ] {
        assert_eq!(
            fx.plane_property(shared, name),
            Some(want),
            "{name} on the handed-over plane is not what the layer asked for"
        );
    }

    fx.teardown();
}

/// Rebinding to another CRTC turns off the planes the scene lit on the old
/// one, and the new output gets planes rather than the canvas.
///
/// Port of `RebindToAnotherCrtcReleasesTheOldPlanes` (`a335bef`,
/// drm-cxx#340). Left lit, the old planes keep the old frame on screen, and a
/// plane both CRTCs can use fails every test that moves it, so upstream's
/// new output composited. The port fixed this first (P-37), through the
/// pending detach the caller commits; the case is the same as upstream's but
/// for that step. Needs two connected outputs on different CRTCs, which
/// `validation/vkms-two-crtc.sh` builds.
#[test]
#[ignore = "needs a DRM device, DRM master and two connected outputs"]
fn a_rebind_to_another_crtc_releases_the_old_planes_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    let Some(other) = fx.another_output() else {
        drmkit_testkit::skipped("no second connected output on another CRTC");
        return;
    };
    let old_crtc = fx.crtc_id();
    let ((aw, ah), (bw, bh)) = (fx.mode_size(), other.mode_size());
    let (w, h) = (aw.min(bw), ah.min(bh));
    for (size, zpos) in [((w, h), 3), ((LAYER_W, LAYER_H), 4)] {
        let Some(handle) = fx.add_layer(0, 0, size.0, size.1) else {
            drmkit_testkit::skipped("no dumb buffer for a layer");
            fx.teardown();
            return;
        };
        fx.set_zpos(handle, zpos);
    }

    let before = fx.lit_planes(old_crtc);
    fx.commit().expect("the cold frame on the first output");
    fx.commit().expect("the steady frame");
    let scene_lit: Vec<u32> = fx
        .lit_planes(old_crtc)
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    assert!(
        !scene_lit.is_empty(),
        "the scene lit nothing on the first CRTC"
    );

    fx.switch_to(&other)
        .expect("learn the second output's planes");
    let (width, height) = fx.mode_size();
    assert!(fx.scene.rebind(fx.crtc_id(), width, height).is_empty());
    fx.commit_detach().expect("the detach commits");
    let report = fx.commit().expect("the first frame on the second output");
    assert_eq!(report.layers_assigned, 2, "{report:?}");
    assert_eq!(
        report.layers_composited, 0,
        "the old CRTC's planes were not handed over: {report:?}"
    );

    let after = fx.lit_planes(old_crtc);
    for id in &scene_lit {
        assert!(
            !after.contains(id),
            "plane {id} still lit on the old CRTC: {after:?}"
        );
    }
    fx.teardown();
}
