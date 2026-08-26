// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The allocator's test-commit economy, against a real device.
//!
//! Ports the assertions from drm-cxx's `allocator_torture` benchmark. They are
//! thresholds on *behaviour*, not wall-clock — how many `TEST_ONLY` commits a
//! frame costs — which is why they belong in the test suite rather than a
//! timing harness: they are deterministic, and a regression in them is a
//! regression in the thing the allocator exists to do.
//!
//! Every extra test commit is an ioctl on the frame path. The plan's numbers:
//! average under 1.5 once warm, exactly 1 in steady state, and zero on a frame
//! that takes the FB-only fast path.
//!
//! Every case is `#[ignore]`d and runs under the vkms lane with
//! `--include-ignored`.

use std::sync::Mutex;

mod common;

use drmkit_core::{AtomicCommitFlags, Device, ObjectType, PropertyStore};
use drmkit_fmt::fourcc;
use drmkit_planes::{Allocator, PlaneCapabilities, PlaneRegistry, PlaneType};
use drmkit_scene::{
    AcquiredBuffer, CommitKind, CommitReport, DeviceCommitter, DisplayParams, KernelResult,
    LayerBufferSource, LayerHandle, LayerScene, PlanePropertyMap, Rect, SourceError, SourceFormat,
};

/// Frames each case runs after warm-up. Long enough that a per-frame
/// regression shows in the average rather than hiding in one bad frame.
const CHURN_FRAMES: usize = 60;
const DRIFT_FRAMES: usize = 40;
/// Frames the allocator is allowed to take to settle after a burst.
const BURST_RECOVERY: usize = 4;

static CARD_LOCK: Mutex<()> = Mutex::new(());

// --- which case pins which mechanism -----------------------------------------
//
// Measured by disabling each caching mechanism in the allocator and seeing
// which cases notice. Recorded because "the suite is green" says nothing about
// whether a case would go red for the reason it is named after, and two of
// these turned out not to.
//
//                                    warm-start off   fast path off   both off
//   a_steady_frame_costs_no_test_...        ok            FAILS         FAILS
//   slow_drift_holds_at_one_test_...      FAILS             ok          FAILS
//   a_burst_settles_within_a_few_...        ok              ok          FAILS
//   rapid_churn_averages_under_the_...      ok              ok            ok
//   n_plus_one_places_every_layer           ok              ok            ok
//
// The first three pin something, and between them cover both mechanisms.
//
// `n_plus_one_places_every_layer` surviving both is correct: it is about the
// matcher placing every layer, not about caching.
//
// `rapid_churn_averages_under_the_threshold` surviving both is P-14. It does
// not merely lack teeth on this hardware -- it asserts nothing about caching
// at all, and would pass against an allocator that searched from scratch every
// frame. It is kept because the threshold is upstream's contract and the
// metric it prints is real, but it is not a regression test for anything.

fn card_guard() -> std::sync::MutexGuard<'static, ()> {
    CARD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn open_card_or_skip() -> Option<Device> {
    let path = std::env::var("DRMKIT_TEST_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_owned());
    drmkit_testkit::announce_card(&path);
    match Device::open(&path) {
        Ok(device) => Some(device),
        Err(error) => {
            assert!(
                std::env::var_os("DRMKIT_REQUIRE_MASTER").is_none(),
                "{path}: {error}, but DRMKIT_REQUIRE_MASTER is set"
            );
            drmkit_testkit::skipped(&format!("no DRM device at {path} ({error})"));
            None
        }
    }
}

/// A dumb-buffer source the composition fallback can also read.
struct Source {
    inner: drmkit_scene_sources::DumbBufferSource,
}

impl LayerBufferSource for Source {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        self.inner.acquire()
    }
    fn release(&mut self, acquired: AcquiredBuffer) {
        self.inner.release(acquired);
    }
    fn format(&self) -> SourceFormat {
        self.inner.format()
    }
    fn map(
        &mut self,
        access: drmkit_dumb::MapAccess,
    ) -> Result<drmkit_dumb::Mapping<'_>, SourceError> {
        self.inner.map(access)
    }
}

struct Rig {
    crtc_index: u32,
    device: Device,
    registry: PlaneRegistry,
    map: PlanePropertyMap,
    scene: LayerScene,
    planes: usize,
}

fn rig() -> Option<Rig> {
    use drm::control::Device as _;

    let device = open_card_or_skip()?;
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

    let resources = device.resource_handles().expect("resources");
    let crtcs = resources.crtcs();
    let (crtc_id, crtc_index) = common::pick_crtc(&device, &resources);

    let mut capabilities = Vec::new();
    for handle in device.plane_handles().expect("planes") {
        let info = device.get_plane(handle).expect("plane info");
        let plane_id: u32 = handle.into();
        let mut mask = 0u32;
        for allowed in resources.filter_crtcs(info.possible_crtcs()) {
            if let Some(index) = crtcs.iter().position(|c| *c == allowed)
                && index < u32::BITS as usize
            {
                mask |= 1 << index;
            }
        }
        let mut store = PropertyStore::new();
        store
            .cache_properties(&device, plane_id, ObjectType::Plane)
            .expect("plane properties");
        let plane_type = match store.property_value(plane_id, "type") {
            Ok(1) => PlaneType::Primary,
            Ok(2) => PlaneType::Cursor,
            _ => PlaneType::Overlay,
        };
        capabilities.push(PlaneCapabilities {
            id: plane_id,
            possible_crtcs: mask,
            plane_type,
            formats: vec![fourcc::ARGB8888],
            supports_scaling: true,
            ..PlaneCapabilities::default()
        });
    }
    let registry = PlaneRegistry::from_capabilities(capabilities);
    let planes = registry.force_disable_candidates(crtc_index).count();
    assert!(planes > 0, "a CRTC with no usable plane cannot display");
    println!("note: {planes} candidate plane(s)");

    let mut map = PlanePropertyMap::new();
    map.learn_all(&device, &registry).expect("learn planes");

    let mut scene = LayerScene::new(crtc_id);
    scene
        .enable_composition(&device, 256, 256)
        .expect("composition canvas");

    Some(Rig {
        crtc_index,
        device,
        registry,
        map,
        scene,
        planes,
    })
}

impl Rig {
    fn add(&mut self, x: i32, y: i32, zpos: u64) -> LayerHandle {
        let source =
            drmkit_scene_sources::DumbBufferSource::create(&self.device, 64, 64, fourcc::ARGB8888)
                .expect("dumb source");
        let handle = self.scene.add_layer(Box::new(Source { inner: source }));
        self.place(handle, x, y, zpos);
        handle
    }

    fn place(&mut self, handle: LayerHandle, x: i32, y: i32, zpos: u64) {
        self.scene
            .layer_mut(handle)
            .expect("layer")
            .set_display(DisplayParams {
                dst_rect: Rect { x, y, w: 64, h: 64 },
                zpos: Some(zpos),
                ..DisplayParams::default()
            });
    }

    /// Run one frame and return its report.
    fn frame(&mut self) -> CommitReport {
        let mut committer = DeviceCommitter::new(
            &self.device,
            &self.map,
            &self.registry,
            self.crtc_index,
            AtomicCommitFlags::empty(),
            None,
        );
        let build = self
            .scene
            .build_frame(
                &self.registry,
                self.crtc_index,
                CommitKind::Real { arms_flip: false },
                &mut committer,
            )
            .expect("build");
        self.scene.finalize_frame(build, KernelResult::Ok)
    }
}

/// **Case 1 — the N+1 problem.** One more layer than there are planes.
///
/// A greedy allocator drops the last layer. Bipartite matching plus the
/// composition fallback places everything: nothing is left unassigned, and at
/// least one layer reaches the screen through the canvas.
#[test]
#[ignore = "needs a DRM device"]
fn n_plus_one_places_every_layer_vkms() {
    let _guard = card_guard();
    let Some(mut rig) = rig() else { return };

    let planes = rig.planes;
    for i in 0..=planes {
        rig.add(0, 0, i as u64);
    }

    let report = rig.frame();
    rig.scene.drain();

    println!(
        "metric: {} layers on {planes} planes -> {} assigned, {} composited, {} unassigned",
        planes + 1,
        report.layers_assigned,
        report.layers_composited,
        report.layers_unassigned
    );
    assert_eq!(
        report.layers_unassigned, 0,
        "every layer must reach the screen, on a plane or via the canvas: {report:?}"
    );
    assert!(
        report.layers_composited > 0,
        "one more layer than planes must put at least one on the canvas"
    );
    assert!(report.accounting_balances());
}

/// **Case 4 — rapid churn.** Geometry moves every frame.
///
/// The warm-start cache has to survive per-frame mutation. Upstream's threshold
/// is an average under 1.5 test commits per frame once warm.
///
/// **This case cannot fail on vkms.** Its planes are interchangeable — same
/// formats, same capabilities — so the greedy search lands on its first try
/// and costs exactly one test commit whether or not a warm cache helped.
/// Disabling the warm start entirely leaves this at 1.00, which I checked
/// rather than assumed, and overflowing the plane count does not change it
/// either. Discriminating needs hardware whose planes differ enough that a
/// cold search has to backtrack; recorded with the other cases vkms cannot
/// reach.
///
/// It is kept because the threshold is upstream's contract and will bite on
/// such hardware — and because the metric it prints is real either way.
#[test]
#[ignore = "needs a DRM device"]
fn rapid_churn_averages_under_the_threshold_vkms() {
    let _guard = card_guard();
    let Some(mut rig) = rig() else { return };

    // One more layer than there are planes, so the frame also exercises
    // composition under churn.
    let layers: Vec<LayerHandle> = (0..=rig.planes)
        .map(|i| rig.add(0, 0, u64::try_from(i).expect("small")))
        .collect();
    rig.frame(); // warm
    rig.scene.flip_landed();

    let mut commits = 0usize;
    for step in 1..=CHURN_FRAMES {
        for (i, handle) in layers.iter().enumerate() {
            let x = i32::try_from(step % 32).expect("small") * 2;
            rig.place(*handle, x, i32::try_from(i).expect("small") * 8, i as u64);
        }
        commits += rig.frame().test_commits_issued;
    }
    rig.scene.drain();

    let average = f64::from(u32::try_from(commits).expect("small"))
        / f64::from(u32::try_from(CHURN_FRAMES).expect("small"));
    println!(
        "metric: rapid churn averaged {average:.2} test commits/frame over {CHURN_FRAMES} frames (threshold 1.5)"
    );
    assert!(
        average < 1.5,
        "warm churn averaged {average:.2} test commits per frame over {CHURN_FRAMES}; \
         the threshold is 1.5, and one search per frame would be 1.0 or more \
         with no cache benefit at all"
    );
}

/// **Case 5 — slow drift.** One layer moves a few pixels per frame.
///
/// Small geometry deltas must keep the warm assignment valid: exactly one test
/// commit per frame, never a full re-search. This is the case that catches an
/// allocator invalidating its cache on any change rather than on a change that
/// matters.
#[test]
#[ignore = "needs a DRM device"]
fn slow_drift_holds_at_one_test_commit_vkms() {
    let _guard = card_guard();
    let Some(mut rig) = rig() else { return };

    let drifting = rig.add(0, 0, 0);
    // A second layer only where there is a plane for it; on a single-plane
    // CRTC the drift is what is being measured, not the composition path.
    if rig.planes > 1 {
        rig.add(96, 0, 1);
    }
    rig.frame(); // warm
    rig.scene.flip_landed();

    let mut worst = 0usize;
    for step in 1..=DRIFT_FRAMES {
        rig.place(drifting, i32::try_from(step * 4).expect("small"), 0, 0);
        worst = worst.max(rig.frame().test_commits_issued);
    }
    rig.scene.drain();

    println!(
        "metric: slow drift peaked at {worst} test commits/frame over {DRIFT_FRAMES} frames (threshold 1)"
    );
    assert!(
        worst <= 1,
        "a four-pixel drift must re-validate the warm assignment, not re-search: \
         worst frame issued {worst} test commits"
    );
}

/// **Case 6 — burst then calm.** Rebuild every layer, then hold still.
///
/// What matters is recovery: after the last mutation the allocator has to be
/// back to one test commit per frame within a few frames, not drifting for the
/// rest of the session.
#[test]
#[ignore = "needs a DRM device"]
fn a_burst_settles_within_a_few_frames_vkms() {
    let _guard = card_guard();
    let Some(mut rig) = rig() else { return };

    let count = rig.planes.clamp(1, 3);
    let layers: Vec<LayerHandle> = (0..count)
        .map(|i| rig.add(0, 0, u64::try_from(i).expect("small")))
        .collect();
    rig.frame();
    rig.scene.flip_landed();

    // The burst: move everything at once.
    for (i, handle) in layers.iter().enumerate() {
        rig.place(
            *handle,
            128,
            i32::try_from(i).expect("small") * 64,
            i as u64,
        );
    }

    let mut settled_at = None;
    for frame in 0..=BURST_RECOVERY + 4 {
        let report = rig.frame();
        if report.test_commits_issued <= 1 && settled_at.is_none() && frame > 0 {
            settled_at = Some(frame);
        }
    }
    rig.scene.drain();

    let settled = settled_at.expect("the scene must settle at all");
    println!("metric: burst settled after {settled} frame(s) (threshold {BURST_RECOVERY})");
    assert!(
        settled <= BURST_RECOVERY,
        "settled after {settled} frames; the threshold is {BURST_RECOVERY}"
    );
}

/// A steady frame costs no test commit at all.
///
/// The floor the other cases are measured against, and invariant 4's promise:
/// when only the framebuffer changed, the kernel has already answered.
#[test]
#[ignore = "needs a DRM device"]
fn a_steady_frame_costs_no_test_commit_vkms() {
    let _guard = card_guard();
    let Some(mut rig) = rig() else { return };

    // As many layers as can actually be placed in one frame, so everything
    // lands on a plane and the frame is a pure fast path. Two hardcoded
    // overflows a CRTC with one usable plane -- which the lane has, and this
    // machine does not.
    //
    // The bound is the smaller of the plane count and the allocator's
    // per-frame test-commit budget. These layers are spatially disjoint, so
    // each forms its own group costing one test commit: on vc4's 17 candidate
    // planes a 17-layer scene spends the default budget of 16 and composites
    // the last one, which is a capacity decision and not the plane shortage
    // this test is about. `a_layer_past_the_budget_is_composited_and_says_so`
    // covers that case deliberately.
    let placeable = rig.planes.min(Allocator::DEFAULT_MAX_TEST_COMMITS);
    for i in 0..placeable {
        rig.add(
            i32::try_from(i).expect("small") * 64,
            0,
            u64::try_from(i).expect("small"),
        );
    }
    rig.frame();
    rig.scene.flip_landed();

    let steady = rig.frame();
    rig.scene.drain();

    assert!(
        steady.fb_delta_fast_path,
        "an unchanged layer set must take the FB-only fast path: {steady:?}"
    );
    assert_eq!(
        steady.test_commits_issued, 0,
        "the fast path exists to skip the test commit entirely"
    );
    assert!(steady.fast_path_consistent());
}

/// Running out of budget is reported as budget, not as a plane shortage.
///
/// Found on a Raspberry Pi 5, whose connected CRTC offers 17 candidate planes
/// -- one more than the default per-frame test-commit budget. Spatially
/// disjoint layers are placed one group at a time at one test commit each, so
/// the seventeenth layer never gets offered the seventeenth plane: it falls
/// through to composition with a free, compatible plane sitting unused. That
/// is a defensible thing to do, and an indefensible thing to do silently --
/// `layers_composited` alone cannot be told apart from "the device is out of
/// planes". vkms has far fewer planes than the budget, so no lane there can
/// reach this.
#[test]
#[ignore = "needs a DRM device with more planes than the test-commit budget"]
fn a_layer_past_the_budget_is_composited_and_says_so_vkms() {
    let _guard = card_guard();
    let Some(mut rig) = rig() else { return };

    let budget = Allocator::DEFAULT_MAX_TEST_COMMITS;
    if rig.planes <= budget {
        drmkit_testkit::skipped(&format!(
            "{} candidate plane(s) does not exceed the budget of {budget}",
            rig.planes
        ));
        return;
    }

    // One layer per plane, all disjoint, so every one of them costs a group.
    for i in 0..rig.planes {
        rig.add(
            i32::try_from(i).expect("small") * 64,
            0,
            u64::try_from(i).expect("small"),
        );
    }
    let report = rig.frame();
    rig.scene.drain();

    assert!(
        report.budget_exhausted,
        "the budget bound this frame and the report has to say so: {report:?}"
    );
    // The budget bounds one search, and a frame that composites runs two: the
    // second holds a plane back for the canvas and re-validates, which is what
    // keeps the canvas inside a tested assignment (P-18). So a budget-bound
    // frame that also composites spends the budget twice, and that is the
    // ceiling.
    assert!(
        report.test_commits_issued >= budget && report.test_commits_issued <= 2 * budget,
        "a budget-bound frame spends between one and two budgets, not {}: {report:?}",
        report.test_commits_issued
    );
    assert!(
        report.layers_composited >= 1,
        "the layers the budget could not reach still have to reach the screen: {report:?}"
    );
    assert_eq!(
        report.layers_unassigned, 0,
        "composited is not dropped -- every layer is accounted for: {report:?}"
    );
}
