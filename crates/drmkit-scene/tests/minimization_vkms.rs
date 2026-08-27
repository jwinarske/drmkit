// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Invariant 4 — minimal property writes — against a real device.
//!
//! Parity port of `tests/integration/test_layer_scene_minimization_vkms.cpp`
//! from drm-cxx @ `4a0b64a`. Every case here is `#[ignore]`d and runs under the
//! vkms lane with `--include-ignored`.
//!
//! The counting is pinned host-side, where the allocator's baseline is set by
//! hand. What these add is that a **real** steady-state frame reaches the
//! minimal path: the fast path was unreachable outside the unit tests once
//! before, because nothing recorded the baseline after a real commit, and the
//! host cases could not see that.

use std::sync::{Mutex, MutexGuard};

use drm::control::Device as ControlDevice;
use drmkit_core::{AtomicCommitFlags, AtomicRequest, Device};
use drmkit_fmt::fourcc;
use drmkit_planes::PlaneRegistry;
use drmkit_scene::{
    CommitKind, CommitReport, DeviceCommitter, DisplayParams, KernelResult, LayerHandle,
    LayerScene, Modeset, PlanePropertyMap, Rect, arm_acquire_fences, emit_frame_damaged,
};

mod common;

static CARD_LOCK: Mutex<()> = Mutex::new(());

fn card_guard() -> MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn open_card() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    drmkit_testkit::announce_card(&path);
    let device = match Device::open(&path) {
        Ok(device) => device,
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            drmkit_testkit::skipped(&format!("no DRM device at {path} ({error})"));
            return None;
        }
    };
    device.enable_universal_planes().expect("universal planes");
    device.enable_atomic().expect("atomic");
    if device.set_master().is_err() {
        assert!(
            std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
            "not DRM master, but DRMKIT_REQUIRE_MASTER is set"
        );
        drmkit_testkit::skipped("another client holds DRM master");
        return None;
    }
    Some(device)
}

const LAYER_W: u32 = 64;
const LAYER_H: u32 = 64;

/// A scene on a connected output, committing for real.
struct Fixture {
    device: Device,
    registry: PlaneRegistry,
    map: PlanePropertyMap,
    scene: LayerScene,
    crtc_id: u32,
    crtc_index: u32,
    connector_id: u32,
    mode: drmkit_core::Mode,
    needs_modeset: bool,
}

fn fixture(device: Device) -> Option<Fixture> {
    let resources = device.resource_handles().ok()?;
    let (connector_id, crtc, mode) = resources.connectors().iter().find_map(|handle| {
        let connector = device.get_connector(*handle, false).ok()?;
        if connector.state() != drm::control::connector::State::Connected {
            return None;
        }
        let mode = *connector.modes().first()?;
        let encoder = connector
            .encoders()
            .iter()
            .find_map(|e| device.get_encoder(*e).ok())?;
        let crtc = resources
            .filter_crtcs(encoder.possible_crtcs())
            .first()
            .copied()?;
        Some((u32::from(*handle), crtc, mode))
    })?;
    let crtc_index = u32::try_from(resources.crtcs().iter().position(|c| *c == crtc)?).ok()?;
    let crtc_id = u32::from(crtc);

    let registry = PlaneRegistry::probe(&device).ok()?;
    let mut map = PlanePropertyMap::new();
    for plane in registry.for_crtc(crtc_index) {
        map.learn_plane(&device, plane.id).ok()?;
    }

    Some(Fixture {
        scene: LayerScene::new(crtc_id),
        device,
        registry,
        map,
        crtc_id,
        crtc_index,
        connector_id,
        mode,
        needs_modeset: true,
    })
}

impl Fixture {
    /// Add a dumb-backed layer at `(x, y)`.
    fn add_layer(&mut self, x: i32, y: i32, w: u32, h: u32) -> Option<LayerHandle> {
        let source =
            drmkit_scene_sources::DumbBufferSource::create(&self.device, w, h, fourcc::XRGB8888)
                .ok()?;
        let handle = self.scene.add_layer(Box::new(source));
        self.scene.layer_mut(handle)?.set_display(DisplayParams {
            src_rect: Rect { x: 0, y: 0, w, h },
            dst_rect: Rect { x, y, w, h },
            ..DisplayParams::default()
        });
        Some(handle)
    }

    /// Move a layer, leaving everything else about it alone.
    fn move_layer(&mut self, handle: LayerHandle, x: i32, y: i32) {
        let layer = self.scene.layer_mut(handle).expect("the layer is there");
        let mut display = *layer.display();
        display.dst_rect = Rect {
            x,
            y,
            w: display.dst_rect.w,
            h: display.dst_rect.h,
        };
        layer.set_display(display);
    }

    /// Build and issue one real commit.
    fn commit(&mut self) -> Result<CommitReport, String> {
        let modeset = if self.needs_modeset {
            Some(
                Modeset::learn(&self.device, self.crtc_id, self.connector_id, &self.mode)
                    .map_err(|error| format!("learning the mode: {error}"))?,
            )
        } else {
            None
        };
        let mut flags = AtomicCommitFlags::empty();
        if modeset.is_some() {
            flags |= AtomicCommitFlags::ALLOW_MODESET;
        }

        let mut committer = DeviceCommitter::new(
            &self.device,
            &self.map,
            &self.registry,
            self.crtc_index,
            AtomicCommitFlags::empty(),
            modeset.as_ref(),
        );
        let mut build = self
            .scene
            .build_frame(
                &self.registry,
                self.crtc_index,
                CommitKind::Real { arms_flip: false },
                &mut committer,
            )
            .map_err(|error| format!("building: {error}"))?;

        let mut request = AtomicRequest::with_capacity(64);
        let (_, _damage_blobs) = emit_frame_damaged(
            &mut request,
            &self.map,
            &mut build,
            modeset.as_ref(),
            Some(&self.device),
        )
        .map_err(|error| format!("emitting: {error}"))?;
        arm_acquire_fences(&mut build, &mut request, &self.map, true)
            .map_err(|error| format!("fences: {error}"))?;

        let programmed: Vec<u32> = build.plan().iter().map(|entry| entry.plane_id).collect();
        match request.commit(&self.device, flags) {
            Ok(()) => {
                // Colorimetry is restated until the kernel has taken it once.
                // Without this it re-emits every frame and the minimal write
                // count is two higher than the diff decided.
                for plane_id in programmed {
                    self.map.note_color_committed(plane_id);
                }
                self.needs_modeset = false;
                Ok(self.scene.finalize_frame(build, KernelResult::Ok))
            }
            Err(error) => {
                self.scene.finalize_frame(build, KernelResult::Rejected);
                Err(format!("committing: {error}"))
            }
        }
    }

    fn teardown(&mut self) {
        let _ = self.device.set_crtc(
            drm::control::crtc::Handle::from(
                std::num::NonZeroU32::new(self.crtc_id).expect("non-zero"),
            ),
            None,
            (0, 0),
            &[],
            None,
        );
    }
}

/// A steady frame writes one property per assigned layer: its `FB_ID`.
///
/// **Invariant 4.** The first commit programs everything and validates it with
/// a `TEST_ONLY`. Once the kernel holds that state, a frame that only changed
/// its buffer has exactly one thing to say — and saying the rest again is
/// per-frame property traffic bought for nothing, on top of a test commit that
/// re-asks a question already answered.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_steady_frame_writes_only_its_fb_id_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    if fx.add_layer(0, 0, LAYER_W, LAYER_H).is_none() {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    }

    let first = fx.commit().expect("the cold frame");
    assert!(
        first.properties_written > 0,
        "the cold frame has to program the plane from nothing"
    );
    assert!(first.fbs_attached >= 1);
    assert!(
        first.test_commits_issued >= 1,
        "a cold placement is a guess until the kernel validates it"
    );

    let second = fx.commit().expect("the steady frame");
    assert_eq!(
        second.properties_written, 1,
        "only FB_ID changed, so only FB_ID is worth writing"
    );
    assert_eq!(second.fbs_attached, 1, "FB_ID re-attaches every frame");
    assert_eq!(
        second.test_commits_issued, 0,
        "the kernel already answered this exact question"
    );
    assert!(second.fb_delta_fast_path, "invariant 4's observable signal");

    fx.teardown();
}

/// Forcing full writes puts every property back on the wire.
///
/// The escape hatch for a driver that mishandles a partial write. It costs
/// exactly what invariant 4's *write* half saves, which is why it is off by
/// default -- and why it has to be reachable, since a caller on such a driver
/// has no other way out.
///
/// It does **not** touch the allocation half. The assignment is still reused
/// and the `TEST_ONLY` still skipped, so `fb_delta_fast_path` stays true: the
/// two are separate mechanisms, and conflating them would have a caller
/// enabling this expecting to pay one test commit per frame as well.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn forcing_full_writes_puts_every_property_back_on_the_wire_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    if fx.add_layer(0, 0, LAYER_W, LAYER_H).is_none() {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    }

    fx.scene.set_force_full_property_writes(true);
    assert!(
        fx.scene.force_full_property_writes(),
        "a setter the getter disagrees with would leave a caller unable to \
         tell whether the quirk is on"
    );

    let first = fx.commit().expect("the cold frame");
    let second = fx.commit().expect("the second frame");

    // Not measured against the cold frame: that one carries the modeset's
    // three properties and the colorimetry restatement, so it is larger for
    // reasons that have nothing to do with the quirk. The minimal path is the
    // baseline that matters, and it writes exactly one -- `FB_ID`.
    assert!(
        second.properties_written > 1,
        "the steady frame wrote {}, which is what the minimal path gives; the \
         quirk is supposed to defeat it",
        second.properties_written
    );
    let _ = first;
    assert!(second.fbs_attached >= 1);
    assert!(
        second.fb_delta_fast_path,
        "the quirk inflates what is written, and nothing else -- the \
         allocation is still reused and the TEST_ONLY still skipped, which is \
         a different mechanism that happens to share the word 'fast'"
    );
    assert_eq!(second.test_commits_issued, 0);

    fx.teardown();
}

/// A layer added to a warm scene still lands on a hardware plane.
///
/// The warm start reuses the previous frame's assignment. A new layer has no
/// previous assignment, so a scene that only ever consulted the warm cache
/// would have nowhere to put it and would fall back to compositing something
/// the hardware could have scanned out directly.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_layer_added_to_a_warm_scene_still_reaches_a_plane_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    if fx.add_layer(0, 0, LAYER_W, LAYER_H).is_none() {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    }

    fx.commit().expect("the cold frame");
    fx.commit().expect("the warm frame");

    if fx.add_layer(32, 32, 32, 32).is_none() {
        drmkit_testkit::skipped("no dumb buffer for a second layer");
        return;
    }
    let report = fx.commit().expect("the frame that adds a layer");

    assert_eq!(report.layers_total, 2);
    assert_eq!(
        report.layers_assigned, 2,
        "both layers must reach scanout on hardware planes"
    );
    assert_eq!(
        report.layers_composited, 0,
        "compositing here would spend a canvas on a layer a free plane could \
         have taken"
    );

    fx.teardown();
}

/// A translated layer writes its rectangle and its `FB_ID`, and nothing else.
///
/// Moving a layer changes `CRTC_X` and `CRTC_Y`, which drmkit emits as one
/// packed rectangle property rather than four. The size, the source rect, the
/// zpos and the rest are unchanged and stay off the wire.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn a_translated_layer_writes_its_rectangle_and_its_fb_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    let Some(handle) = fx.add_layer(32, 32, LAYER_W, LAYER_H) else {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    };

    fx.commit().expect("the cold frame");
    fx.move_layer(handle, 64, 32);
    let report = fx.commit().expect("the frame that moves it");

    assert_eq!(
        report.properties_written, 2,
        "the destination rectangle and FB_ID -- nothing about this layer's \
         size, source or stacking changed"
    );
    assert_eq!(report.fbs_attached, 1, "FB_ID always re-emits");

    fx.teardown();
}

/// A placement change defeats the fast path, and the frame after recovers it.
///
/// The fast path is only safe while the kernel's answer still applies. Moving
/// a layer asks a question the previous `TEST_ONLY` did not answer -- the new
/// position may exceed a scaler, overlap differently, or fall outside what the
/// plane can address -- so it has to be re-validated. A fast path that
/// survived a move would be committing an unvalidated placement.
#[test]
#[ignore = "needs a DRM device and DRM master"]
fn moving_a_layer_defeats_the_fast_path_and_the_next_frame_recovers_it_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };
    let Some(mut fx) = fixture(device) else {
        drmkit_testkit::skipped("no connected output");
        return;
    };
    let Some(handle) = fx.add_layer(32, 32, LAYER_W, LAYER_H) else {
        drmkit_testkit::skipped("no dumb buffer for a layer");
        return;
    };

    fx.commit().expect("the cold frame");
    let steady = fx.commit().expect("the steady frame");
    assert_eq!(steady.test_commits_issued, 0);
    assert!(steady.fb_delta_fast_path);

    fx.move_layer(handle, 64, 32);
    let moved = fx.commit().expect("the frame that moves it");
    assert!(
        !moved.fb_delta_fast_path,
        "a placement change must defeat the fast path"
    );
    assert!(
        moved.test_commits_issued >= 1,
        "a moved layer is an unvalidated placement until the kernel says \
         otherwise"
    );

    let resteady = fx.commit().expect("the frame after");
    assert!(
        resteady.fb_delta_fast_path,
        "the new placement is validated now, so the fast path comes back -- a \
         move that disabled it permanently would cost a test commit every \
         frame thereafter"
    );
    assert_eq!(resteady.test_commits_issued, 0);

    fx.teardown();
}

/// The kernel accepts a damage blob in the layout drmkit builds.
///
/// vkms exposes `FB_DAMAGE_CLIPS` on no plane, so nothing here can commit one
/// — recorded as P-33 and P-34. What *is* reachable is the half that does not
/// need a plane: whether `create_property_blob` takes the `drm_mode_rect`
/// array as laid out. A blob the kernel refuses would fail every damaged
/// frame on the hardware that does expose the property, and finding that out
/// on a board rather than here would be a waste of the board.
#[test]
#[ignore = "needs a DRM device"]
fn the_kernel_takes_a_damage_blob_in_the_layout_we_build_vkms() {
    let _guard = card_guard();
    let Some(device) = open_card() else { return };

    // Two rectangles, as `x1, y1, x2, y2` corner pairs.
    let rects: [[i32; 4]; 2] = [[0, 0, 8, 8], [100, 200, 116, 216]];
    let blob = device
        .create_property_blob(rects.as_slice())
        .expect("the kernel must take a drm_mode_rect array");
    assert_ne!(blob.id(), 0, "a zero blob id names nothing");

    // A single rectangle is the common case and must work too: a length the
    // kernel rounds or rejects would show up here rather than on a board.
    let one: [[i32; 4]; 1] = [[4, 4, 12, 12]];
    let single = device
        .create_property_blob(one.as_slice())
        .expect("one rectangle is a valid blob");
    assert_ne!(single.id(), 0);
    assert_ne!(
        single.id(),
        blob.id(),
        "two live blobs must be distinguishable, or a frame would point at \
         the previous frame's damage"
    );
}
