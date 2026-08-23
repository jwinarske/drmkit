// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Tests for the scene's contract surface.
//!
//! The release *timing* contracts (invariants 1 and 2) need a real page flip
//! and land with the commit path, in the vkms lane. What is testable here is
//! the shape those contracts are built on: that a source's flow-control return
//! is distinguishable from a failure, that the defaults are the safe ones, and
//! that the commit report's accounting is checkable rather than merely
//! documented.

use super::*;
use drmkit_planes::LayerId;

/// A source that can be told what to do, standing in for a live producer.
#[derive(Debug, Default)]
struct FakeSource {
    format: SourceFormat,
    /// Returned by the next `acquire`.
    next: Option<Result<AcquiredBuffer, SourceError>>,
    released: Vec<u32>,
    released_with_fence: usize,
    fresh: bool,
}

impl LayerBufferSource for FakeSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        self.next.take().unwrap_or(Err(SourceError::WouldBlock))
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.released.push(acquired.fb_id);
    }

    fn release_with_fence(
        &mut self,
        acquired: AcquiredBuffer,
        _release_fence: Option<drmkit_sync::SyncFence>,
    ) {
        self.released_with_fence += 1;
        self.release(acquired);
    }

    fn has_fresh_content(&self) -> bool {
        self.fresh
    }

    fn format(&self) -> SourceFormat {
        self.format
    }
}

/// A source that overrides nothing, to exercise the trait's defaults.
#[derive(Debug, Default)]
struct MinimalSource;

impl LayerBufferSource for MinimalSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        Ok(AcquiredBuffer {
            fb_id: 7,
            ..AcquiredBuffer::default()
        })
    }

    fn release(&mut self, _acquired: AcquiredBuffer) {}

    fn format(&self) -> SourceFormat {
        SourceFormat::default()
    }
}

// --- DR-6: EAGAIN is a variant, not an I/O error -----------------------------

/// Flow control must be distinguishable from failure at the type level.
///
/// A source with nothing to contribute this vblank is skipped and retried; a
/// source that actually failed aborts the commit. Recovering that distinction
/// by squinting at an I/O error kind is how the contract gets lost in a
/// refactor — and losing it turns a stalled producer into an aborted commit.
#[test]
fn would_block_is_distinct_from_failure() {
    let mut source = FakeSource::default();

    // Nothing queued: the fake reports flow control.
    assert!(matches!(source.acquire(), Err(SourceError::WouldBlock)));

    source.next = Some(Err(SourceError::Failed(rustix::io::Errno::INVAL)));
    assert!(matches!(source.acquire(), Err(SourceError::Failed(_))));

    source.next = Some(Err(SourceError::Unsupported));
    assert!(matches!(source.acquire(), Err(SourceError::Unsupported)));
}

/// The variants must not be collapsible by accident: a caller matching on
/// `WouldBlock` must not also catch a real failure.
#[test]
fn a_real_failure_is_not_mistaken_for_flow_control() {
    let failure = SourceError::Failed(rustix::io::Errno::AGAIN);
    assert!(
        !matches!(failure, SourceError::WouldBlock),
        "an errno that happens to be EAGAIN is still a failure if the source \
         reported it as one -- the variant carries the intent, not the number"
    );
}

// --- trait defaults: the conservative answers --------------------------------

/// `has_fresh_content` defaults to true, so a source that cannot tell whether
/// its buffer changed never causes a real update to be skipped.
#[test]
fn fresh_content_defaults_to_the_safe_answer() {
    assert!(
        MinimalSource.has_fresh_content(),
        "a source that cannot introspect its buffer must report changed, or a \
         real update is silently skipped"
    );

    // A source that *knows* it is idle opts out, which is what enables the
    // whole-commit skip.
    let idle = FakeSource::default();
    assert!(!idle.has_fresh_content());
}

#[test]
fn binding_model_defaults_to_scene_submits_fb_id() {
    assert_eq!(
        MinimalSource.binding_model(),
        BindingModel::SceneSubmitsFbId
    );
    assert_eq!(BindingModel::default(), BindingModel::SceneSubmitsFbId);
}

#[test]
fn release_fence_is_opt_in() {
    assert!(
        !MinimalSource.wants_release_fence(),
        "the scene only requests an OUT_FENCE when a source asks, so this must \
         cost nothing by default"
    );
}

/// The default `release_with_fence` forwards to `release`, so a source that
/// does not care about the fence still sees every buffer come back.
#[test]
fn release_with_fence_forwards_by_default() {
    let mut source = MinimalSource;
    // Compiles and does not panic: the default drops the fence and forwards.
    source.release_with_fence(
        AcquiredBuffer {
            fb_id: 3,
            ..AcquiredBuffer::default()
        },
        None,
    );
}

/// A source that overrides it sees the fence path instead.
#[test]
fn release_with_fence_is_overridable() {
    let mut source = FakeSource::default();
    source.release_with_fence(
        AcquiredBuffer {
            fb_id: 5,
            ..AcquiredBuffer::default()
        },
        None,
    );
    assert_eq!(source.released_with_fence, 1);
    assert_eq!(source.released, vec![5]);
}

/// Both composition escapes default to unsupported, which is what makes a
/// source uncompositable rather than silently producing garbage.
#[test]
fn composition_escapes_default_to_unsupported() {
    let mut source = MinimalSource;
    assert!(matches!(
        source.map(drmkit_dumb::MapAccess::Read),
        Err(SourceError::Unsupported)
    ));
    assert!(matches!(
        source.export_dma_buf(),
        Err(SourceError::Unsupported)
    ));
}

/// The driver-binding hooks are no-ops by default (DR-4 dormant surface), so a
/// source that submits its own `FB_ID` needs to know nothing about them.
#[test]
fn driver_binding_hooks_are_inert_by_default() {
    let mut source = MinimalSource;
    assert!(source.bind_to_plane(31).is_ok());
    source.unbind_from_plane(31);
    source.on_session_paused();
}

// --- AcquiredBuffer ----------------------------------------------------------

/// Moving transfers the buffer intact.
///
/// The *non-copyability* this depends on is pinned by a `compile_fail` doctest
/// on [`AcquiredBuffer`] itself, not here: no stable Rust assertion can express
/// "this type must not be `Copy`", and a test function that silently asserts
/// nothing is worse than no test.
#[test]
fn moving_an_acquired_buffer_transfers_it_intact() {
    let buffer = AcquiredBuffer {
        fb_id: 42,
        token: 3,
        ..AcquiredBuffer::default()
    };
    let moved = buffer;
    assert_eq!(moved.fb_id, 42);
    assert_eq!(moved.token, 3);
}

#[test]
fn an_acquired_buffer_defaults_to_full_frame_damage() {
    let buffer = AcquiredBuffer::default();
    assert!(
        buffer.damage.is_empty(),
        "empty means full-frame: correct, just not power-optimal"
    );
    assert!(buffer.acquire_fence.is_none());
}

// --- CommitReport ------------------------------------------------------------

/// The accounting invariant the C++ states as a comment, made checkable.
#[test]
fn commit_report_accounting_balances() {
    let report = CommitReport {
        layers_total: 5,
        layers_assigned: 2,
        layers_composited: 1,
        layers_unassigned: 1,
        layers_skipped_no_frame: 1,
        ..CommitReport::default()
    };
    assert!(report.accounting_balances());

    let unbalanced = CommitReport {
        layers_total: 5,
        layers_assigned: 2,
        ..CommitReport::default()
    };
    assert!(
        !unbalanced.accounting_balances(),
        "a layer counted nowhere must show up as an imbalance"
    );
}

/// A pin that could not be honored does not disturb the accounting: the layer
/// falls back to normal allocation and is still counted once.
#[test]
fn unhonored_pins_do_not_affect_the_accounting() {
    let report = CommitReport {
        layers_total: 1,
        layers_assigned: 1,
        pin_requests_unhonored: 1,
        ..CommitReport::default()
    };
    assert!(report.accounting_balances());
}

/// **Invariant 4's observable signal.** Taking the fast path means issuing no
/// test commits; a report claiming both is self-contradictory.
#[test]
fn fast_path_implies_no_test_commits() {
    let good = CommitReport {
        fb_delta_fast_path: true,
        test_commits_issued: 0,
        ..CommitReport::default()
    };
    assert!(good.fast_path_consistent());

    let contradictory = CommitReport {
        fb_delta_fast_path: true,
        test_commits_issued: 1,
        ..CommitReport::default()
    };
    assert!(!contradictory.fast_path_consistent());

    // Not taking the fast path says nothing about the count.
    let warm = CommitReport {
        fb_delta_fast_path: false,
        test_commits_issued: 1,
        ..CommitReport::default()
    };
    assert!(warm.fast_path_consistent());
}

#[test]
fn an_empty_report_is_balanced_and_consistent() {
    let report = CommitReport::default();
    assert!(report.accounting_balances());
    assert!(report.fast_path_consistent());
    assert!(report.placements.is_empty());
    assert!(!report.fb_delta_fast_path);
}

#[test]
fn placements_describe_each_layer_outcome() {
    let report = CommitReport {
        layers_total: 2,
        layers_assigned: 1,
        layers_unassigned: 1,
        placements: vec![
            LayerPlacement {
                layer: LayerId(1),
                placement: Placement::AssignedToPlane,
                plane_id: Some(31),
                plane_rotation_bits: 0b0001,
            },
            LayerPlacement {
                layer: LayerId(2),
                placement: Placement::Unassigned,
                plane_id: None,
                plane_rotation_bits: 0,
            },
        ],
        ..CommitReport::default()
    };

    assert!(report.accounting_balances());
    assert_eq!(report.placements.len(), 2);
    assert_eq!(
        report.placements[1].plane_id, None,
        "unassigned has no plane"
    );
    assert_eq!(
        report.placements[1].plane_rotation_bits, 0,
        "and no rotation mask to report"
    );
}

// --- the deferred-release ring: invariants 1, 2 and 5 ------------------------

fn acquisition(layer: u64, fb_id: u32) -> Acquisition {
    Acquisition::new(
        LayerId(layer),
        AcquiredBuffer {
            fb_id,
            ..AcquiredBuffer::default()
        },
    )
}

/// The framebuffer ids returned, in order.
fn released_fbs(released: &Released) -> Vec<u32> {
    released
        .acquisitions
        .iter()
        .map(|a| a.buffer.fb_id)
        .collect()
}

/// **Invariant 1.** A buffer is released only after the flip that stops
/// scanning it out completes — two commits deep, never at ioctl return.
///
/// Frame 1 and frame 2 must return nothing: frame 1's buffer is still on
/// screen when frame 2 commits. Only frame 3 retires it.
#[test]
fn release_is_deferred_two_commits_deep() {
    let mut queue = ReleaseQueue::new();

    let first = queue.commit_succeeded(vec![acquisition(1, 100)], None, |_| false);
    assert!(
        first.is_empty(),
        "frame 1's buffer is being scanned out; releasing it here is the tear"
    );

    let second = queue.commit_succeeded(vec![acquisition(1, 200)], None, |_| false);
    assert!(
        second.is_empty(),
        "frame 1's buffer only leaves the screen when frame 2's flip completes, \
         which the *next* commit observes"
    );

    let third = queue.commit_succeeded(vec![acquisition(1, 300)], None, |_| false);
    assert_eq!(
        released_fbs(&third),
        vec![100],
        "two commits deep: frame 1 retires now"
    );
    assert_eq!(third.reason, ReleaseReason::Retired);

    let fourth = queue.commit_succeeded(vec![acquisition(1, 400)], None, |_| false);
    assert_eq!(released_fbs(&fourth), vec![200], "then frame 2, in order");
}

/// The ring holds exactly two generations, however long it runs.
#[test]
fn the_ring_holds_exactly_two_generations() {
    let mut queue = ReleaseQueue::new();
    assert!(queue.is_empty());

    for frame in 0..20u32 {
        queue.commit_succeeded(vec![acquisition(1, frame)], None, |_| false);
        assert!(
            queue.in_flight() <= 2,
            "frame {frame}: {} buffers in flight, expected at most 2",
            queue.in_flight()
        );
    }
    assert_eq!(queue.in_flight(), 2, "steady state holds N-1 and N-2");
}

/// **Invariant 2.** A failed commit releases that frame's acquisitions
/// immediately — no flip is in flight to gate them on, and a self-healing
/// producer ring depends on reusing them at once.
#[test]
fn a_failed_commit_releases_its_own_acquisitions_at_once() {
    let mut queue = ReleaseQueue::new();
    queue.commit_succeeded(vec![acquisition(1, 100)], None, |_| false);

    let failed = queue.commit_failed(vec![acquisition(1, 200)]);

    assert_eq!(
        released_fbs(&failed),
        vec![200],
        "this frame's buffer comes straight back"
    );
    assert_eq!(failed.reason, ReleaseReason::CommitFailed);
}

/// A failed commit must **not** disturb what is on screen. The previous
/// commit's buffers are still being scanned out — a rejection displaced
/// nothing.
#[test]
fn a_failed_commit_leaves_the_in_flight_generations_alone() {
    let mut queue = ReleaseQueue::new();
    queue.commit_succeeded(vec![acquisition(1, 100)], None, |_| false);
    queue.commit_succeeded(vec![acquisition(1, 200)], None, |_| false);
    let before = queue.in_flight();

    queue.commit_failed(vec![acquisition(1, 300)]);
    assert_eq!(queue.in_flight(), before, "nothing on screen changed");

    // And the ring is unbroken: the next success still retires frame 1.
    let next = queue.commit_succeeded(vec![acquisition(1, 400)], None, |_| false);
    assert_eq!(
        released_fbs(&next),
        vec![100],
        "a rejected commit must not skip a generation"
    );
}

/// A test commit applies nothing to hardware, so its buffers come back at once
/// too — reported distinctly, because the caller counts them differently.
#[test]
fn a_test_commit_releases_immediately_and_is_distinguishable() {
    let mut queue = ReleaseQueue::new();
    let released = queue.test_committed(vec![acquisition(1, 100)]);

    assert_eq!(released_fbs(&released), vec![100]);
    assert_eq!(released.reason, ReleaseReason::TestOnly);
    assert!(queue.is_empty(), "a test commit holds nothing in flight");
}

/// **Invariant 5's counterpart.** Draining returns everything held, oldest
/// first, for teardown and session pause — paths with no flip in flight, where
/// holding buffers back would simply leak them.
#[test]
fn draining_returns_every_held_buffer_oldest_first() {
    let mut queue = ReleaseQueue::new();
    queue.commit_succeeded(vec![acquisition(1, 100)], None, |_| false);
    queue.commit_succeeded(vec![acquisition(1, 200)], None, |_| false);
    assert_eq!(queue.in_flight(), 2);

    let drained = queue.drain();

    assert_eq!(
        released_fbs(&drained),
        vec![100, 200],
        "oldest first, matching the order a running scene would have used"
    );
    assert_eq!(drained.reason, ReleaseReason::Drained);
    assert!(queue.is_empty(), "draining leaves nothing behind");
}

#[test]
fn draining_an_empty_queue_is_harmless() {
    let mut queue = ReleaseQueue::new();
    let drained = queue.drain();
    assert!(drained.is_empty());
    assert_eq!(drained.len(), 0);
}

/// Multiple layers rotate together: a frame's buffers retire as a set.
#[test]
fn every_layers_buffer_retires_together() {
    let mut queue = ReleaseQueue::new();

    queue.commit_succeeded(vec![acquisition(1, 100), acquisition(2, 101)], None, |_| {
        false
    });
    queue.commit_succeeded(vec![acquisition(1, 200), acquisition(2, 201)], None, |_| {
        false
    });
    let third =
        queue.commit_succeeded(vec![acquisition(1, 300), acquisition(2, 301)], None, |_| {
            false
        });

    let mut fbs = released_fbs(&third);
    fbs.sort_unstable();
    assert_eq!(
        fbs,
        vec![100, 101],
        "both layers' frame-1 buffers retire together"
    );
}

/// A layer that skipped a frame — its source returned `WouldBlock` — simply
/// contributes nothing to that generation, and the ring stays consistent.
#[test]
fn a_skipped_layer_does_not_break_the_rotation() {
    let mut queue = ReleaseQueue::new();

    queue.commit_succeeded(vec![acquisition(1, 100), acquisition(2, 101)], None, |_| {
        false
    });
    // Layer 2 had no frame this vblank.
    queue.commit_succeeded(vec![acquisition(1, 200)], None, |_| false);
    let third = queue.commit_succeeded(vec![acquisition(1, 300)], None, |_| false);

    let mut fbs = released_fbs(&third);
    fbs.sort_unstable();
    assert_eq!(fbs, vec![100, 101]);

    let fourth = queue.commit_succeeded(vec![acquisition(1, 400)], None, |_| false);
    assert_eq!(
        released_fbs(&fourth),
        vec![200],
        "only layer 1 contributed to frame 2"
    );
}

/// The release fence is stamped only onto sources that asked for it, so a scene
/// with no interested source pays nothing.
#[test]
fn the_release_fence_is_stamped_only_where_wanted() {
    use std::os::fd::AsFd as _;

    let mut queue = ReleaseQueue::new();
    queue.commit_succeeded(vec![acquisition(1, 100), acquisition(2, 101)], None, |_| {
        false
    });

    // An eventfd stands in for the commit's OUT_FENCE.
    let raw = rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC).expect("eventfd");
    let fence = drmkit_sync::SyncFence::import(raw.as_fd()).expect("import");

    // Only layer 1 wants it.
    queue.commit_succeeded(vec![acquisition(1, 200)], Some(&fence), |layer| {
        layer == LayerId(1)
    });

    // Frame 1's buffers retire next commit; check what came back stamped.
    let retired = queue.commit_succeeded(vec![acquisition(1, 300)], None, |_| false);
    let stamped: Vec<(u32, bool)> = retired
        .acquisitions
        .iter()
        .map(|a| (a.buffer.fb_id, a.release_fence.is_some()))
        .collect();

    assert_eq!(stamped.len(), 2);
    assert!(
        stamped.contains(&(100, true)),
        "layer 1 opted in and must get the fence: {stamped:?}"
    );
    assert!(
        stamped.contains(&(101, false)),
        "layer 2 did not opt in and must not: {stamped:?}"
    );
}

/// No fence offered means nothing stamped, whatever the sources want.
#[test]
fn no_out_fence_means_nothing_is_stamped() {
    let mut queue = ReleaseQueue::new();
    queue.commit_succeeded(vec![acquisition(1, 100)], None, |_| true);
    queue.commit_succeeded(vec![acquisition(1, 200)], None, |_| true);

    let retired = queue.commit_succeeded(vec![acquisition(1, 300)], None, |_| true);
    assert!(
        retired
            .acquisitions
            .iter()
            .all(|a| a.release_fence.is_none()),
        "a CRTC without OUT_FENCE_PTR stamps nothing"
    );
}

// --- invariant 5: the armed-flip tripwire ------------------------------------

/// Dropping a scene with a flip still armed is the RmFB-on-in-flight-FB hazard.
/// Tracking it is what lets the scene's drop say so out loud.
#[test]
fn pending_flip_tracks_whether_teardown_is_safe() {
    let mut pending = ScenePendingFlip::new();
    assert!(!pending.is_armed(), "nothing committed yet");

    pending.arm();
    assert!(
        pending.is_armed(),
        "a commit that armed PAGE_FLIP_EVENT still references its framebuffer"
    );

    pending.landed();
    assert!(!pending.is_armed(), "once dispatched, teardown is safe");
}
