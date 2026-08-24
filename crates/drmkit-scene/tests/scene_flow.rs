// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! End-to-end scene flow, with no device.
//!
//! The pieces built separately — the source contract, the release ring, the
//! frame lifecycle, lowering, and the allocator — run together here for the
//! first time. Everything is host-side: the allocator's test commits go to a
//! fake, and the kernel's answer to a real commit is a parameter.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use drmkit_fmt::fourcc;
use drmkit_planes::{
    LayerRef, PlaneCapabilities, PlaneRegistry, PlaneType, TestCommitter, TestFailure,
};
use drmkit_scene::{
    AcquiredBuffer, CommitKind, DisplayParams, KernelResult, LayerBufferSource, LayerScene, Rect,
    SceneError, SourceError, SourceFormat,
};

// --- fixtures ----------------------------------------------------------------

fn registry() -> PlaneRegistry {
    PlaneRegistry::from_capabilities(vec![
        PlaneCapabilities {
            id: 31,
            possible_crtcs: 0b1,
            plane_type: PlaneType::Primary,
            formats: vec![fourcc::XRGB8888],
            supports_scaling: true,
            ..PlaneCapabilities::default()
        },
        PlaneCapabilities {
            id: 32,
            possible_crtcs: 0b1,
            plane_type: PlaneType::Overlay,
            formats: vec![fourcc::XRGB8888],
            supports_scaling: true,
            ..PlaneCapabilities::default()
        },
    ])
}

/// What a source did, observable from the test.
#[derive(Debug, Default)]
struct SourceLog {
    acquired: usize,
    released: Vec<u32>,
}

/// A source whose behavior the test controls.
struct TestSource {
    log: Rc<RefCell<SourceLog>>,
    /// Framebuffer id handed out next.
    next_fb: u32,
    /// Return `WouldBlock` instead of a buffer.
    starved: bool,
    /// Fail for a real reason.
    fails: bool,
    /// Starve on demand, after the source has been handed to the scene.
    ///
    /// `starved` is fixed at construction, which suits a source that never
    /// produces. A layer that starves *after* putting a frame up is a
    /// different case, and the interesting one: it has something to hold.
    toggle: Rc<Cell<bool>>,
}

impl TestSource {
    fn new(log: &Rc<RefCell<SourceLog>>) -> Self {
        Self {
            log: Rc::clone(log),
            next_fb: 100,
            starved: false,
            fails: false,
            toggle: Rc::new(Cell::new(false)),
        }
    }

    /// A handle to this source's starve switch, to keep before boxing it.
    fn starve_switch(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.toggle)
    }
}

impl LayerBufferSource for TestSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        if self.fails {
            return Err(SourceError::Failed(rustix::io::Errno::IO));
        }
        if self.starved || self.toggle.get() {
            return Err(SourceError::WouldBlock);
        }
        self.log.borrow_mut().acquired += 1;
        let fb_id = self.next_fb;
        self.next_fb += 1;
        Ok(AcquiredBuffer {
            fb_id,
            ..AcquiredBuffer::default()
        })
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.log.borrow_mut().released.push(acquired.fb_id);
    }

    fn format(&self) -> SourceFormat {
        SourceFormat {
            fourcc: fourcc::XRGB8888,
            modifier: 0,
            width: 1920,
            height: 1080,
        }
    }
}

/// Accepts every test commit.
#[derive(Debug, Default)]
struct Accepting {
    calls: usize,
}

impl TestCommitter for Accepting {
    fn test_assignment(&mut self, _assignment: &[(u32, LayerRef<'_>)]) -> Result<(), TestFailure> {
        self.calls += 1;
        Ok(())
    }
}

fn full_screen() -> DisplayParams {
    DisplayParams {
        dst_rect: Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        },
        ..DisplayParams::default()
    }
}

fn real() -> CommitKind {
    CommitKind::Real { arms_flip: true }
}

// --- the flow ----------------------------------------------------------------

#[test]
fn a_layer_is_acquired_lowered_placed_and_released() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let handle = scene.add_layer(Box::new(TestSource::new(&log)));
    scene
        .layer_mut(handle)
        .expect("layer")
        .set_display(full_screen());

    let registry = registry();
    let mut committer = Accepting::default();

    // Frame 1.
    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("build");
    assert_eq!(build.held(), 1, "the source contributed a buffer");
    let report = scene.finalize_frame(build, KernelResult::Ok);

    assert_eq!(report.layers_total, 1);
    assert_eq!(report.layers_assigned, 1, "one plane, one layer");
    assert_eq!(report.layers_skipped_no_frame, 0);
    assert!(report.accounting_balances());
    assert_eq!(log.borrow().acquired, 1);
    assert!(
        log.borrow().released.is_empty(),
        "invariant 1: the buffer is on screen, not back yet"
    );
    assert!(scene.has_pending_flip());

    // Frames 2 and 3: frame 1's buffer comes back on the third.
    scene.flip_landed();
    for _ in 0..2 {
        let build = scene
            .build_frame(&registry, 0, real(), &mut committer)
            .expect("build");
        scene.finalize_frame(build, KernelResult::Ok);
        scene.flip_landed();
    }
    assert_eq!(
        log.borrow().released,
        vec![100],
        "invariant 1: released two commits deep, in order"
    );

    scene.drain();
}

/// A starved source is skipped and counted — never a failure, and the frame
/// still commits.
#[test]
fn a_starved_source_is_skipped_not_failed() {
    let live_log = Rc::new(RefCell::new(SourceLog::default()));
    let starved_log = Rc::new(RefCell::new(SourceLog::default()));

    let mut scene = LayerScene::new(1);
    let live = scene.add_layer(Box::new(TestSource::new(&live_log)));
    let mut starved_source = TestSource::new(&starved_log);
    starved_source.starved = true;
    scene.add_layer(Box::new(starved_source));

    scene
        .layer_mut(live)
        .expect("layer")
        .set_display(full_screen());

    let mut committer = Accepting::default();
    let build = scene
        .build_frame(&registry(), 0, real(), &mut committer)
        .expect("a starved source must not abort the build");
    let report = scene.finalize_frame(build, KernelResult::Ok);

    assert_eq!(report.layers_total, 2);
    assert_eq!(report.layers_skipped_no_frame, 1);
    assert_eq!(report.layers_assigned, 1);
    assert!(report.accounting_balances());
    assert_eq!(starved_log.borrow().acquired, 0);

    scene.flip_landed();
    scene.drain();
}

/// A source that fails for a real reason aborts the build — and the buffers
/// already taken this pass go straight back, so those rings do not stall.
#[test]
fn a_failing_source_aborts_the_build_and_returns_what_was_taken() {
    let good_log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    scene.add_layer(Box::new(TestSource::new(&good_log)));
    let mut failing = TestSource::new(&Rc::new(RefCell::new(SourceLog::default())));
    failing.fails = true;
    scene.add_layer(Box::new(failing));

    let mut committer = Accepting::default();
    let result = scene.build_frame(&registry(), 0, real(), &mut committer);

    assert!(
        matches!(result, Err(SceneError::Source(SourceError::Failed(_)))),
        "a real source failure must surface as such, not as a skip: {result:?}"
    );
    assert_eq!(
        good_log.borrow().released,
        vec![100],
        "the buffer taken before the failure must not be stranded"
    );
}

/// **Invariant 2**, end to end: a rejected commit returns that frame's buffers
/// at once and leaves the scene running.
#[test]
fn a_rejected_commit_returns_buffers_and_keeps_the_scene_running() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let handle = scene.add_layer(Box::new(TestSource::new(&log)));
    scene
        .layer_mut(handle)
        .expect("layer")
        .set_display(full_screen());

    let registry = registry();
    let mut committer = Accepting::default();

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("build");
    scene.finalize_frame(build, KernelResult::Rejected);

    assert_eq!(
        log.borrow().released,
        vec![100],
        "the rejected frame's buffer comes straight back"
    );
    assert!(!scene.is_suspended(), "a rejection is recoverable");

    // And the scene still commits next frame.
    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("the scene kept running");
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();
    scene.drain();
}

/// Lost DRM master suspends the scene, and further builds short-circuit until
/// the session resumes.
#[test]
fn lost_master_suspends_until_resumed() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let handle = scene.add_layer(Box::new(TestSource::new(&log)));
    scene
        .layer_mut(handle)
        .expect("layer")
        .set_display(full_screen());

    let registry = registry();
    let mut committer = Accepting::default();

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("build");
    scene.finalize_frame(build, KernelResult::NotMaster);
    assert!(scene.is_suspended());

    assert!(
        matches!(
            scene.build_frame(&registry, 0, real(), &mut committer),
            Err(SceneError::Suspended)
        ),
        "a suspended scene short-circuits with Suspended, not some other error"
    );

    scene.resume();
    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("resumed");
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();
    scene.drain();
}

// --- handles -----------------------------------------------------------------

/// A handle to a removed layer must never address whatever takes its slot.
#[test]
fn a_stale_handle_does_not_address_the_slots_new_occupant() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);

    let first = scene.add_layer(Box::new(TestSource::new(&log)));
    assert!(scene.layer(first).is_some());

    assert!(scene.remove_layer(first));
    assert!(scene.layer(first).is_none(), "removed");

    // The slot is recycled by the next add.
    let second = scene.add_layer(Box::new(TestSource::new(&log)));
    assert!(scene.layer(second).is_some());
    assert!(
        scene.layer(first).is_none(),
        "the stale handle must not resolve to the new occupant"
    );
    assert_ne!(
        first.layer_id(),
        second.layer_id(),
        "and their allocator identities must differ, or warm-start state would \
         carry across two unrelated layers"
    );

    assert!(!scene.remove_layer(first), "removing twice is a no-op");
}

/// Removing a layer whose buffers are still in flight keeps its source alive
/// until they come back — releasing to a dropped source would strand them.
#[test]
fn a_removed_layers_source_survives_until_its_buffers_return() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let handle = scene.add_layer(Box::new(TestSource::new(&log)));
    scene
        .layer_mut(handle)
        .expect("layer")
        .set_display(full_screen());

    let registry = registry();
    let mut committer = Accepting::default();

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("build");
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();
    assert!(log.borrow().released.is_empty());

    // Remove it while its buffer is still on screen.
    assert!(scene.remove_layer(handle));
    assert!(scene.is_empty());

    // Draining must still reach the retired source.
    scene.drain();
    assert_eq!(
        log.borrow().released,
        vec![100],
        "the buffer must reach the source that produced it, not vanish"
    );
}

#[test]
fn an_empty_scene_builds_an_empty_frame() {
    let mut scene = LayerScene::new(1);
    let mut committer = Accepting::default();

    let build = scene
        .build_frame(&registry(), 0, real(), &mut committer)
        .expect("build");
    let report = scene.finalize_frame(build, KernelResult::Ok);

    assert_eq!(report.layers_total, 0);
    assert!(report.accounting_balances());

    scene.flip_landed();
}

/// A changed hint drops the warm start so the layer can move to the plane it
/// now prefers.
#[test]
fn changing_a_hint_forces_reallocation() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let handle = scene.add_layer(Box::new(TestSource::new(&log)));
    scene
        .layer_mut(handle)
        .expect("layer")
        .set_display(full_screen());

    let registry = registry();
    let mut committer = Accepting::default();

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("build");
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();

    scene
        .layer_mut(handle)
        .expect("layer")
        .set_content_type(drmkit_planes::ContentType::Video);
    assert!(scene.layer(handle).expect("layer").hints_dirty());

    let calls_before = committer.calls;
    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("build");
    assert!(
        committer.calls > calls_before,
        "a changed hint must force a re-search, not reuse the cached assignment"
    );
    assert!(
        !scene.layer(handle).expect("layer").hints_dirty(),
        "and the flag clears once the allocation has seen it"
    );
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();
    scene.drain();
}

/// The invariant 5 tripwire is real: dropping a scene with a flip still armed
/// is the RmFB-on-in-flight-FB hazard, and the scene says so.
///
/// This caught two cases in this very file while they were being written —
/// both drained but never landed the flip, which is precisely the mistake a
/// caller makes.
#[test]
#[should_panic(expected = "page-flip event still armed")]
fn dropping_a_scene_with_an_armed_flip_is_caught() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let handle = scene.add_layer(Box::new(TestSource::new(&log)));
    scene
        .layer_mut(handle)
        .expect("layer")
        .set_display(full_screen());

    let mut committer = Accepting::default();
    let build = scene
        .build_frame(&registry(), 0, real(), &mut committer)
        .expect("build");
    scene.finalize_frame(build, KernelResult::Ok);
    assert!(scene.has_pending_flip());

    // Dropped here without landing the flip. In a release build this is silent
    // and shows up as a rare tear; in development it is loud, which is the
    // whole point of the tripwire.
    drop(scene);
}

/// A commit that armed no flip leaves teardown safe, so no tripwire fires.
#[test]
fn a_scene_without_an_armed_flip_drops_cleanly() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let handle = scene.add_layer(Box::new(TestSource::new(&log)));
    scene
        .layer_mut(handle)
        .expect("layer")
        .set_display(full_screen());

    let mut committer = Accepting::default();
    let build = scene
        .build_frame(
            &registry(),
            0,
            CommitKind::Real { arms_flip: false },
            &mut committer,
        )
        .expect("build");
    scene.finalize_frame(build, KernelResult::Ok);

    assert!(!scene.has_pending_flip());
    scene.drain();
}

// --- invariant 4: the fast path has to be reachable from the scene -----------

/// A steady frame issues no test commit at all.
///
/// **Invariant 4.** The allocator's FB-only fast path is gated on a baseline of
/// what the kernel last accepted, and only a successful *real* commit may set
/// it. Nothing in the scene recorded that baseline until the parity harness
/// counted the test commits the reference did not issue: the allocator's own
/// unit tests populate the baseline by calling `record_committed` themselves,
/// so they passed while the path was unreachable in production.
///
/// Counting commits from outside the allocator is the point. A test that sets
/// up the baseline by hand proves the predicate works and says nothing about
/// whether anyone ever satisfies it.
#[test]
fn a_steady_frame_issues_no_test_commit() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let handle = scene.add_layer(Box::new(TestSource::new(&log)));
    scene
        .layer_mut(handle)
        .expect("layer")
        .set_display(full_screen());

    let registry = registry();
    let mut committer = Accepting::default();

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("first frame");
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();
    let after_first = committer.calls;
    assert!(
        after_first > 0,
        "the first frame has no baseline and must search"
    );

    // Nothing changed but the buffer, which is what a compositor does every
    // frame it is not reconfiguring anything.
    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("second frame");
    // Read the fact out before finalizing, and assert only once the scene is
    // quiescent. Asserting with a flip still armed makes the failure unwind
    // through `LayerScene::drop`, whose invariant-5 tripwire then fires during
    // cleanup -- and a panic in a destructor while panicking aborts the
    // process, so the message that mattered never gets printed.
    let took_fast_path = build.report().fb_delta_fast_path;
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();

    assert!(
        took_fast_path,
        "an unchanged layer set must take the FB-only fast path"
    );
    assert_eq!(
        committer.calls,
        after_first,
        "a steady frame must not issue a test commit; it issued {}",
        committer.calls - after_first
    );
}

/// Removing a layer does not cost a test commit.
///
/// Dropping a layer only frees resources, so the assignment the kernel already
/// accepted stays valid without it. The scene used to invalidate the whole
/// warm-start cache here, which forced a full search -- several test commits --
/// for a configuration that could not have become invalid.
#[test]
fn removing_a_layer_does_not_force_a_search() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let keep = scene.add_layer(Box::new(TestSource::new(&log)));
    let drop_me = scene.add_layer(Box::new(TestSource::new(&log)));
    for handle in [keep, drop_me] {
        scene
            .layer_mut(handle)
            .expect("layer")
            .set_display(full_screen());
    }

    let registry = registry();
    let mut committer = Accepting::default();

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("first frame");
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();
    let after_first = committer.calls;

    scene.remove_layer(drop_me);

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("frame after removal");
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();

    assert_eq!(
        committer.calls, after_first,
        "removing a layer must not re-test the survivors"
    );
}

// --- invariant 4: the steady-state property-write census ---------------------

/// A steady frame re-disables nothing and diffs everything.
///
/// **Invariant 4.** Once the kernel has taken a configuration, the next frame's
/// job is to hand it new buffers, not to restate the configuration. Two things
/// have to hold for that: every assigned plane must carry a baseline to diff
/// against, and no plane the kernel already has off may be disabled again.
///
/// Re-disabling costs two property writes per idle plane per frame, which on a
/// card with ten planes is most of the frame's property traffic and none of its
/// meaning.
#[test]
fn a_steady_frame_disables_nothing_and_diffs_everything() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let handle = scene.add_layer(Box::new(TestSource::new(&log)));
    scene
        .layer_mut(handle)
        .expect("layer")
        .set_display(full_screen());

    let registry = registry();
    let mut committer = Accepting::default();

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("first frame");
    assert!(
        build.plan().iter().all(|entry| entry.baseline.is_none()),
        "the first frame has no baseline and must write every property"
    );
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("second frame");
    let all_have_baselines = build.plan().iter().all(|entry| entry.baseline.is_some());
    let disables = build.disables().to_vec();
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();

    assert!(
        all_have_baselines,
        "a plane the kernel already accepted must be diffed, not re-stated"
    );
    assert!(
        disables.is_empty(),
        "no plane changed hands, so nothing needs disabling; got {disables:?}"
    );
}

/// The plane a removed layer was using still gets switched off.
///
/// Dropping the layer removes the scene's reason to program that plane, but the
/// kernel is still scanning out whatever was on it. The frame after a removal
/// has to disable it explicitly or it keeps displaying the departed layer's
/// last framebuffer.
///
/// This is the hazard in pruning the allocator's committed baseline on removal:
/// the baseline entry is the only record that the plane is still lit, so
/// erasing it makes the plane look already-off and nothing disables it. The
/// entry is kept and its layer identity cleared instead.
#[test]
fn the_plane_a_removed_layer_held_is_disabled() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let keep = scene.add_layer(Box::new(TestSource::new(&log)));
    let drop_me = scene.add_layer(Box::new(TestSource::new(&log)));
    for handle in [keep, drop_me] {
        scene
            .layer_mut(handle)
            .expect("layer")
            .set_display(full_screen());
    }

    let registry = registry();
    let mut committer = Accepting::default();

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("first frame");
    let vacated = build
        .plan()
        .iter()
        .find(|entry| entry.layer_id == drop_me.layer_id())
        .map(|entry| entry.plane_id)
        .expect("the layer being dropped must have been placed");
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();

    scene.remove_layer(drop_me);

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("frame after removal");
    let disables = build.disables().to_vec();
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();

    assert!(
        disables.contains(&vacated),
        "plane {vacated} still holds the removed layer's framebuffer and was \
         not disabled; disables were {disables:?}"
    );
}

// --- EAGAIN flow control -----------------------------------------------------

/// A starved layer keeps its plane, and the report still balances.
///
/// `WouldBlock` from a source is flow control, not failure: the layer has no
/// new frame this vblank and goes on showing the one it already put up. It
/// must not be dropped from the commit, because a layer dropped from the
/// commit has its plane disabled — a source that hiccups for one frame would
/// blink the layer off screen.
///
/// Holding the plane is what makes the accounting delicate. The layer is in
/// the assignment *and* counted as skipped, so a report that took
/// `layers_assigned` straight from the assignment counts it twice and the
/// identity
///
/// ```text
/// total = assigned + composited + unassigned + skipped
/// ```
///
/// stops holding. A compositor watching those counters for dropped frames then
/// sees phantom ones.
#[test]
fn a_starved_layer_holds_its_plane_and_the_report_still_balances() {
    let log = Rc::new(RefCell::new(SourceLog::default()));
    let mut scene = LayerScene::new(1);
    let steady = scene.add_layer(Box::new(TestSource::new(&log)));
    let hiccup_source = TestSource::new(&log);
    let switch = hiccup_source.starve_switch();
    let hiccup = scene.add_layer(Box::new(hiccup_source));
    for handle in [steady, hiccup] {
        scene
            .layer_mut(handle)
            .expect("layer")
            .set_display(full_screen());
    }

    let registry = registry();
    let mut committer = Accepting::default();

    // One good frame, so the layer has something to hold.
    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("first frame");
    let plane_of_hiccup = build
        .plan()
        .iter()
        .find(|entry| entry.layer_id == hiccup.layer_id())
        .map(|entry| entry.plane_id)
        .expect("both layers must be placed");
    scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();

    switch.set(true);

    let build = scene
        .build_frame(&registry, 0, real(), &mut committer)
        .expect("starved frame");
    let still_programmed = build
        .plan()
        .iter()
        .any(|entry| entry.plane_id == plane_of_hiccup);
    let disabled = build.disables().contains(&plane_of_hiccup);
    let report = scene.finalize_frame(build, KernelResult::Ok);
    scene.flip_landed();

    assert!(
        still_programmed,
        "a starved layer must keep its plane and re-attach the frame it \
         already had"
    );
    assert!(
        !disabled,
        "plane {plane_of_hiccup} was switched off; the layer would blink"
    );
    assert_eq!(
        report.layers_skipped_no_frame, 1,
        "the starved layer must be counted as skipped"
    );
    assert!(
        report.accounting_balances(),
        "report double-counts the starved layer: total {} != assigned {} + \
         composited {} + unassigned {} + skipped {}",
        report.layers_total,
        report.layers_assigned,
        report.layers_composited,
        report.layers_unassigned,
        report.layers_skipped_no_frame
    );

    scene.drain();
}
