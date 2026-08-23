// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! End-to-end scene flow, with no device.
//!
//! The pieces built separately — the source contract, the release ring, the
//! frame lifecycle, lowering, and the allocator — run together here for the
//! first time. Everything is host-side: the allocator's test commits go to a
//! fake, and the kernel's answer to a real commit is a parameter.

use std::cell::RefCell;
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
}

impl TestSource {
    fn new(log: &Rc<RefCell<SourceLog>>) -> Self {
        Self {
            log: Rc::clone(log),
            next_fb: 100,
            starved: false,
            fails: false,
        }
    }
}

impl LayerBufferSource for TestSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        if self.fails {
            return Err(SourceError::Failed(rustix::io::Errno::IO));
        }
        if self.starved {
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
