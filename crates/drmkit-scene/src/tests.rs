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

// --- lowering ----------------------------------------------------------------

use drmkit_planes::{Layer as PlaneLayer, PropTag};

fn lowering_input() -> LoweringInput {
    LoweringInput {
        display: DisplayParams {
            dst_rect: Rect {
                x: 10,
                y: 20,
                w: 640,
                h: 480,
            },
            ..DisplayParams::default()
        },
        format: SourceFormat {
            fourcc: 0x3234_5258, // XR24
            modifier: 0,
            width: 1920,
            height: 1080,
        },
        binding: BindingModel::SceneSubmitsFbId,
        fb_id: 42,
        crtc_id: 7,
        default_zpos: None,
    }
}

#[test]
fn lowering_writes_the_destination_rectangle_in_whole_pixels() {
    let mut plane_layer = PlaneLayer::new();
    lower_layer(&lowering_input(), &mut plane_layer);

    assert_eq!(plane_layer.property(PropTag::CrtcX), Some(10));
    assert_eq!(plane_layer.property(PropTag::CrtcY), Some(20));
    assert_eq!(plane_layer.property(PropTag::CrtcW), Some(640));
    assert_eq!(plane_layer.property(PropTag::CrtcH), Some(480));
}

/// A zero-sized source rectangle means "the buffer's full extent".
///
/// Callers commonly set only `dst_rect` — the scene cannot guess a screen
/// position but can read the source extent from the format. Without this
/// resolution `SRC_W`/`SRC_H` go out as 0, which the kernel rejects with
/// `EINVAL`; the C++ records that as the root cause of direct scanout failing
/// on every controller it was tried on, sending every frame through
/// composition.
#[test]
fn an_unset_source_rectangle_resolves_to_the_buffer_extent() {
    let mut plane_layer = PlaneLayer::new();
    lower_layer(&lowering_input(), &mut plane_layer);

    assert_eq!(
        plane_layer.property(PropTag::SrcW),
        Some(to_16_16(1920)),
        "an unset source width must become the buffer width, never 0"
    );
    assert_eq!(plane_layer.property(PropTag::SrcH), Some(to_16_16(1080)));
    assert_ne!(plane_layer.property(PropTag::SrcW), Some(0));
}

#[test]
fn an_explicit_source_rectangle_is_kept_and_converted_to_fixed_point() {
    let mut input = lowering_input();
    input.display.src_rect = Rect {
        x: 8,
        y: 16,
        w: 320,
        h: 240,
    };

    let mut plane_layer = PlaneLayer::new();
    lower_layer(&input, &mut plane_layer);

    assert_eq!(plane_layer.property(PropTag::SrcX), Some(to_16_16(8)));
    assert_eq!(plane_layer.property(PropTag::SrcY), Some(to_16_16(16)));
    assert_eq!(plane_layer.property(PropTag::SrcW), Some(320 << 16));
    assert_eq!(plane_layer.property(PropTag::SrcH), Some(240 << 16));
}

/// `CRTC_ID` must always be written. Without it the kernel rejects the plane
/// commit — the framebuffer is armed but the plane is still bound to whatever
/// the previous commit left — so every allocator test fails and the scene
/// reports zero layers assigned.
#[test]
fn crtc_id_is_always_written() {
    let mut plane_layer = PlaneLayer::new();
    lower_layer(&lowering_input(), &mut plane_layer);
    assert_eq!(plane_layer.property(PropTag::CrtcId), Some(7));

    // Even for a driver-owned binding, which skips only FB_ID.
    let mut input = lowering_input();
    input.binding = BindingModel::DriverOwnsBinding;
    let mut driver_owned = PlaneLayer::new();
    lower_layer(&input, &mut driver_owned);
    assert_eq!(driver_owned.property(PropTag::CrtcId), Some(7));
}

/// A driver-owned binding gets its `FB_ID` from the producer's extension stack,
/// so writing it here would race the consumer's internal state. The layer is
/// also flagged externally bound so the allocator skips it.
#[test]
fn a_driver_owned_binding_skips_fb_id_and_is_marked_external() {
    let mut input = lowering_input();
    input.binding = BindingModel::DriverOwnsBinding;

    let mut plane_layer = PlaneLayer::new();
    lower_layer(&input, &mut plane_layer);

    assert_eq!(
        plane_layer.property(PropTag::FbId),
        None,
        "the scene must not write FB_ID for a driver-owned binding"
    );
    assert!(plane_layer.is_externally_bound());
}

#[test]
fn a_scene_owned_binding_writes_fb_id_and_is_not_external() {
    let mut plane_layer = PlaneLayer::new();
    lower_layer(&lowering_input(), &mut plane_layer);

    assert_eq!(plane_layer.property(PropTag::FbId), Some(42));
    assert!(!plane_layer.is_externally_bound());
}

/// Format and modifier reach the bag so the allocator can screen planes before
/// issuing any test commit.
#[test]
fn format_and_modifier_reach_the_property_bag() {
    let mut input = lowering_input();
    input.format.modifier = 0x0800_0000_0000_0001; // AFBC(16x16)

    let mut plane_layer = PlaneLayer::new();
    lower_layer(&input, &mut plane_layer);

    assert_eq!(plane_layer.format(), Some(0x3234_5258));
    assert_eq!(plane_layer.modifier(), 0x0800_0000_0000_0001);
}

/// **`zpos` is written only when asked for.** Emitting 0 unconditionally would
/// statically disqualify any primary plane with an immutable non-zero `zpos` —
/// amdgpu pins primary at 2 — leaving a single-layer scene with nowhere to go.
#[test]
fn zpos_is_left_unwritten_unless_requested() {
    let mut plane_layer = PlaneLayer::new();
    lower_layer(&lowering_input(), &mut plane_layer);
    assert_eq!(
        plane_layer.property(PropTag::Zpos),
        None,
        "an unrequested zpos must not be written as 0"
    );

    let mut explicit = lowering_input();
    explicit.display.zpos = Some(3);
    let mut with_zpos = PlaneLayer::new();
    lower_layer(&explicit, &mut with_zpos);
    assert_eq!(with_zpos.property(PropTag::Zpos), Some(3));
}

/// The scene's hint fills in only when the caller expressed no preference.
#[test]
fn the_default_zpos_hint_only_applies_when_unset() {
    let mut hinted = lowering_input();
    hinted.default_zpos = Some(1);
    let mut plane_layer = PlaneLayer::new();
    lower_layer(&hinted, &mut plane_layer);
    assert_eq!(plane_layer.property(PropTag::Zpos), Some(1));

    // An explicit request wins over the hint.
    hinted.display.zpos = Some(4);
    let mut explicit = PlaneLayer::new();
    lower_layer(&hinted, &mut explicit);
    assert_eq!(explicit.property(PropTag::Zpos), Some(4));
}

/// Rotation is likewise conditional: zero means "no rotation", and writing it
/// would claim a capability the plane may not have.
#[test]
fn rotation_is_written_only_when_non_zero() {
    let mut plane_layer = PlaneLayer::new();
    lower_layer(&lowering_input(), &mut plane_layer);
    assert_eq!(plane_layer.property(PropTag::Rotation), None);

    let mut rotated = lowering_input();
    rotated.display.rotation = 0b0010;
    let mut with_rotation = PlaneLayer::new();
    lower_layer(&rotated, &mut with_rotation);
    assert_eq!(with_rotation.property(PropTag::Rotation), Some(0b0010));
}

#[test]
fn alpha_is_written_only_when_requested() {
    let mut plane_layer = PlaneLayer::new();
    lower_layer(&lowering_input(), &mut plane_layer);
    assert_eq!(plane_layer.property(PropTag::Alpha), None);

    let mut translucent = lowering_input();
    translucent.display.alpha = Some(0x8000);
    let mut with_alpha = PlaneLayer::new();
    lower_layer(&translucent, &mut with_alpha);
    assert_eq!(with_alpha.property(PropTag::Alpha), Some(0x8000));
}

/// A negative destination position round-trips: a plane may extend off-screen,
/// and KMS carries `CRTC_X`/`CRTC_Y` as signed values in unsigned slots.
#[test]
fn a_negative_destination_position_round_trips() {
    let mut input = lowering_input();
    input.display.dst_rect.x = -64;
    input.display.dst_rect.y = -32;

    let mut plane_layer = PlaneLayer::new();
    lower_layer(&input, &mut plane_layer);

    let rect = plane_layer.crtc_rect();
    assert_eq!(rect.x, -64, "a plane may extend off the left edge");
    assert_eq!(rect.y, -32);
}

/// A lowered layer must be scale-detectable: the allocator screens planes on
/// it, and getting the fixed-point conversion wrong would make every layer look
/// scaled.
#[test]
fn a_one_to_one_layer_does_not_read_as_scaled() {
    let mut input = lowering_input();
    input.display.dst_rect = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1080,
    };

    let mut plane_layer = PlaneLayer::new();
    lower_layer(&input, &mut plane_layer);
    assert!(
        !plane_layer.requires_scaling(),
        "a full-extent source into a matching destination is 1:1"
    );

    // And a genuine downscale does.
    input.display.dst_rect.w = 1280;
    let mut scaled = PlaneLayer::new();
    lower_layer(&input, &mut scaled);
    assert!(scaled.requires_scaling());
}

// --- frame lifecycle: what a commit does to scene state ----------------------

fn real_commit() -> CommitKind {
    CommitKind::Real { arms_flip: true }
}

/// A successful real commit rotates the ring — invariant 1 end to end through
/// the lifecycle rather than the queue alone.
#[test]
fn a_successful_commit_rotates_and_arms_the_flip() {
    let mut lifecycle = FrameLifecycle::new();

    let first = lifecycle.finalize(
        real_commit(),
        KernelResult::Ok,
        vec![acquisition(1, 100)],
        CommitReport::default(),
        None,
        |_| false,
    );
    assert!(first.released.is_empty(), "nothing has left the screen yet");
    assert!(lifecycle.has_pending_flip(), "a real commit armed a flip");
    assert!(!first.suspended);

    lifecycle.finalize(
        real_commit(),
        KernelResult::Ok,
        vec![acquisition(1, 200)],
        CommitReport::default(),
        None,
        |_| false,
    );
    let third = lifecycle.finalize(
        real_commit(),
        KernelResult::Ok,
        vec![acquisition(1, 300)],
        CommitReport::default(),
        None,
        |_| false,
    );
    assert_eq!(released_fbs(&third.released), vec![100]);
}

/// A commit that does not arm a flip leaves teardown safe.
#[test]
fn a_commit_without_a_flip_event_leaves_no_pending_flip() {
    let mut lifecycle = FrameLifecycle::new();
    lifecycle.finalize(
        CommitKind::Real { arms_flip: false },
        KernelResult::Ok,
        vec![acquisition(1, 100)],
        CommitReport::default(),
        None,
        |_| false,
    );
    assert!(!lifecycle.has_pending_flip());
}

/// **Invariant 2, both clauses.** A non-`EACCES` rejection releases this
/// frame's buffers at once and does **not** suspend the scene. Suspending on a
/// recoverable stumble would stop a scene that should have healed next frame.
#[test]
fn an_ordinary_rejection_releases_but_does_not_suspend() {
    let mut lifecycle = FrameLifecycle::new();
    lifecycle.finalize(
        real_commit(),
        KernelResult::Ok,
        vec![acquisition(1, 100)],
        CommitReport::default(),
        None,
        |_| false,
    );

    let rejected = lifecycle.finalize(
        real_commit(),
        KernelResult::Rejected,
        vec![acquisition(1, 200)],
        CommitReport::default(),
        None,
        |_| false,
    );

    assert_eq!(released_fbs(&rejected.released), vec![200]);
    assert_eq!(rejected.released.reason, ReleaseReason::CommitFailed);
    assert!(
        !rejected.suspended,
        "a non-EACCES failure must not suspend the scene -- the frame heals"
    );
    assert!(!lifecycle.is_suspended());
}

/// Lost DRM master is the one failure that suspends.
#[test]
fn lost_master_suspends_the_scene() {
    let mut lifecycle = FrameLifecycle::new();

    let outcome = lifecycle.finalize(
        real_commit(),
        KernelResult::NotMaster,
        vec![acquisition(1, 100)],
        CommitReport::default(),
        None,
        |_| false,
    );

    assert!(outcome.suspended);
    assert!(lifecycle.is_suspended());
    assert_eq!(
        released_fbs(&outcome.released),
        vec![100],
        "the buffers still come straight back"
    );

    lifecycle.resume();
    assert!(!lifecycle.is_suspended(), "a session resume lifts it");
}

/// **Invariant 4's safety net.** The kernel rejected a frame whose `TEST_ONLY`
/// was skipped, so the cached allocation is no longer trustworthy: drop it and
/// re-search next frame. The frame is dropped but heals — never corruption.
#[test]
fn a_rejected_fast_path_frame_invalidates_the_cached_allocation() {
    let mut lifecycle = FrameLifecycle::new();
    let report = CommitReport {
        fb_delta_fast_path: true,
        test_commits_issued: 0,
        ..CommitReport::default()
    };

    let outcome = lifecycle.finalize(
        real_commit(),
        KernelResult::Rejected,
        vec![acquisition(1, 100)],
        report,
        None,
        |_| false,
    );

    assert!(
        outcome.invalidate_allocation,
        "a fast-path frame the kernel refused must drop the cache"
    );
    assert!(
        !outcome.report.fb_delta_fast_path,
        "and the report must stop claiming a fast path that did not hold"
    );
    assert!(lifecycle.has_warned_fast_path_reject());
    assert!(!outcome.suspended, "it is still a recoverable rejection");
}

/// A rejection *after* a real test says nothing new about the cache, so it must
/// not invalidate it — that would throw away a good assignment every time the
/// kernel refused for an unrelated reason.
#[test]
fn an_ordinary_rejection_does_not_invalidate_the_cache() {
    let mut lifecycle = FrameLifecycle::new();
    let report = CommitReport {
        fb_delta_fast_path: false,
        test_commits_issued: 1,
        ..CommitReport::default()
    };

    let outcome = lifecycle.finalize(
        real_commit(),
        KernelResult::Rejected,
        vec![acquisition(1, 100)],
        report,
        None,
        |_| false,
    );

    assert!(!outcome.invalidate_allocation);
    assert!(!lifecycle.has_warned_fast_path_reject());
}

/// A test commit changes nothing: no rotation, no flip, no suspension.
#[test]
fn a_test_commit_leaves_scene_state_untouched() {
    let mut lifecycle = FrameLifecycle::new();
    lifecycle.finalize(
        real_commit(),
        KernelResult::Ok,
        vec![acquisition(1, 100)],
        CommitReport::default(),
        None,
        |_| false,
    );
    let in_flight = lifecycle.buffers_in_flight();
    lifecycle.flip_landed();

    let outcome = lifecycle.finalize(
        CommitKind::Test,
        KernelResult::Ok,
        vec![acquisition(1, 200)],
        CommitReport::default(),
        None,
        |_| false,
    );

    assert_eq!(released_fbs(&outcome.released), vec![200]);
    assert_eq!(outcome.released.reason, ReleaseReason::TestOnly);
    assert_eq!(
        lifecycle.buffers_in_flight(),
        in_flight,
        "a test applied nothing, so nothing rotated"
    );
    assert!(!lifecycle.has_pending_flip(), "and armed no flip");
}

/// Even a *failed* test releases immediately and does not suspend — the kind
/// decides the handling, not the result.
#[test]
fn a_failed_test_commit_is_still_just_a_test() {
    let mut lifecycle = FrameLifecycle::new();
    let outcome = lifecycle.finalize(
        CommitKind::Test,
        KernelResult::NotMaster,
        vec![acquisition(1, 100)],
        CommitReport::default(),
        None,
        |_| false,
    );

    assert_eq!(outcome.released.reason, ReleaseReason::TestOnly);
    assert!(
        !outcome.suspended,
        "a probe that could not run is not a reason to stop the scene"
    );
}

/// **Invariant 5.** The tripwire tracks whether teardown is safe, and draining
/// does not clear it — landing the flip is the caller's job.
#[test]
fn draining_does_not_land_the_flip() {
    let mut lifecycle = FrameLifecycle::new();
    lifecycle.finalize(
        real_commit(),
        KernelResult::Ok,
        vec![acquisition(1, 100)],
        CommitReport::default(),
        None,
        |_| false,
    );
    assert!(lifecycle.has_pending_flip());

    let drained = lifecycle.drain();
    assert_eq!(drained.reason, ReleaseReason::Drained);
    assert_eq!(lifecycle.buffers_in_flight(), 0);
    assert!(
        lifecycle.has_pending_flip(),
        "draining returns buffers; it does not wait on the kernel, so the flip \
         is still outstanding and teardown is still unsafe"
    );

    lifecycle.flip_landed();
    assert!(!lifecycle.has_pending_flip());
}

// --- EAGAIN accounting -------------------------------------------------------

/// A source with nothing to contribute is skipped and counted, never treated as
/// a failure — the flow-control half of DR-6, at the accounting level.
#[test]
fn the_acquire_tally_separates_skips_from_acquisitions() {
    let mut tally = AcquireTally::default();

    assert!(tally.record(true));
    assert!(!tally.record(false));
    assert!(tally.record(true));

    assert_eq!(tally.acquired, 2);
    assert_eq!(tally.skipped_no_frame, 1);
    assert_eq!(
        tally.considered(),
        3,
        "every layer is accounted for exactly once"
    );
}

/// The tally is what keeps the commit report's accounting balanced: a skipped
/// layer belongs in `layers_skipped_no_frame`, not in the failure tallies.
#[test]
fn skipped_layers_keep_the_report_balanced() {
    let mut tally = AcquireTally::default();
    tally.record(true);
    tally.record(false);
    tally.record(false);

    let report = CommitReport {
        layers_total: tally.considered(),
        layers_assigned: tally.acquired,
        layers_skipped_no_frame: tally.skipped_no_frame,
        ..CommitReport::default()
    };
    assert!(
        report.accounting_balances(),
        "a skipped layer must be counted somewhere, or the invariant breaks"
    );
}

// --- acquire fences ----------------------------------------------------------

/// A plane that takes `IN_FENCE_FD` gets the fence.
///
/// `AcquiredBuffer::acquire_fence` means the pixels are not valid yet. Giving
/// the descriptor to KMS is what makes the kernel hold scanout until the
/// producer signals; without it a half-rendered frame reaches the screen with
/// nothing having waited.
#[test]
fn a_plane_that_takes_a_fence_is_armed_with_it() {
    assert_eq!(
        crate::fence_action(Some(77)),
        crate::FenceAction::Arm { property_id: 77 },
    );
}

/// A plane that does not falls back to waiting on the CPU.
///
/// There is nowhere to put the fence, so the wait has to happen before the
/// commit instead. Slower, and the whole point: the alternative is not "no
/// wait", it is scanning out a buffer the producer has not finished.
///
/// This branch is why the decision is a function rather than an `if` inside
/// the arming loop. vkms advertises `IN_FENCE_FD` on all ten of its planes, so
/// no test against it can ever reach the fallback.
#[test]
fn a_plane_without_the_property_falls_back_to_a_cpu_wait() {
    assert_eq!(crate::fence_action(None), crate::FenceAction::CpuWait);
}

/// A derived zpos is bent to fit the plane, not sent to be refused.
///
/// Found on a Raspberry Pi 5: vc4's overlays advertise `zpos [1, 17]`, the
/// composition canvas asked for 19 -- one above the 18 layers in the scene --
/// and the kernel refused the entire frame with `EINVAL`, all sixteen
/// correctly programmed layers along with it. vkms exposes no zpos property at
/// all, so nothing there ever writes one and no lane could have caught it.
#[test]
fn a_canvas_zpos_above_the_plane_range_is_clamped_not_sent() {
    let mut map = PlanePropertyMap::new();
    map.zpos_range.insert(7, (1, 17));

    assert_eq!(
        map.clamp_zpos(7, 19),
        17,
        "above the range clamps to the top"
    );
    assert_eq!(
        map.clamp_zpos(7, 0),
        1,
        "below the range clamps to the bottom"
    );
    assert_eq!(map.clamp_zpos(7, 9), 9, "inside the range is left alone");
    assert_eq!(
        map.clamp_zpos(7, 17),
        17,
        "the top of the range is inside it"
    );

    // A plane that advertises no range is not second-guessed: the value goes
    // as-is and the kernel decides, which is what happens on vkms.
    assert_eq!(
        map.clamp_zpos(8, 19),
        19,
        "no known range means no clamping"
    );
}

/// A zpos fixed at one value, which the kernel does not flag immutable.
///
/// This is P-6, and the hardware is real: two Allwinner boards in the drmdb
/// corpus advertise `zpos` with `min == max == 0` and leave it mutable, so
/// `PlanePropertyMap` keeps it in the writable set. Writing anything but 0 to
/// such a plane is `EINVAL`, which refuses the whole frame.
///
/// The reference guards it separately, with `zpos_is_fixed`, because it sends
/// the requested value through unchanged. Here the clamp added for P-15
/// already covers it: a degenerate range admits exactly one value, so the
/// derived zpos becomes a write the kernel takes.
///
/// If the clamp is ever narrowed to skip degenerate ranges as a no-op, this
/// is what says those two boards stop scanning out.
#[test]
fn a_degenerate_zpos_clamps_to_the_only_value_it_will_take() {
    let mut map = PlanePropertyMap::new();
    map.zpos_range.insert(42, (0, 0));

    assert_eq!(
        map.clamp_zpos(42, 3),
        0,
        "the only legal value, not the one that would be refused"
    );
    assert_eq!(map.clamp_zpos(42, 0), 0, "and asking for it is unchanged");
}

// --- the primary anchor (P-26) ----------------------------------------------

/// A plane for the reservation cases, with a zpos range if it has one.
fn reservation_plane(
    id: u32,
    plane_type: drmkit_planes::PlaneType,
    zpos: Option<(u64, u64)>,
) -> drmkit_planes::PlaneCapabilities {
    drmkit_planes::PlaneCapabilities {
        id,
        possible_crtcs: 0b1,
        plane_type,
        formats: vec![drmkit_fmt::fourcc::ARGB8888, drmkit_fmt::fourcc::XRGB8888],
        zpos_min: zpos.map(|(min, _)| min),
        zpos_max: zpos.map(|(_, max)| max),
        ..drmkit_planes::PlaneCapabilities::default()
    }
}

/// A layer carrying an explicit zpos, or none.
fn reservation_layer(zpos: Option<u64>) -> drmkit_planes::Layer {
    let mut layer = drmkit_planes::Layer::default();
    if let Some(zpos) = zpos {
        layer.set_property(drmkit_planes::PropTag::Zpos, zpos);
    }
    layer
}

/// **P-26.** A pinned primary nothing can land on is reserved, not left to the
/// disable pass.
///
/// amdgpu pins its primary at zpos 2 and refuses a commit that disables the
/// primary of an active CRTC. With every layer sitting above the pin -- which
/// is the layout the reference's own examples use, `zpos >= 3` -- nothing is
/// eligible for the primary, so nothing assigns it, so the disable pass clears
/// it in every test and the kernel refuses every one. The allocator places
/// nothing and the entire scene falls through to software composition, still
/// correct, still arriving, silently not using the hardware.
#[test]
fn a_pinned_primary_with_no_eligible_layer_is_reserved() {
    let registry = drmkit_planes::PlaneRegistry::from_capabilities(vec![
        reservation_plane(31, drmkit_planes::PlaneType::Primary, Some((2, 2))),
        reservation_plane(32, drmkit_planes::PlaneType::Overlay, Some((3, 9))),
        reservation_plane(33, drmkit_planes::PlaneType::Overlay, Some((3, 9))),
    ]);
    let layers = [reservation_layer(Some(3)), reservation_layer(Some(4))];
    let refs: Vec<drmkit_planes::LayerRef<'_>> = layers
        .iter()
        .enumerate()
        .map(|(i, layer)| drmkit_planes::LayerRef {
            id: drmkit_planes::LayerId(i as u64 + 1),
            layer,
        })
        .collect();

    assert_eq!(
        crate::scene::canvas_reservation(true, &refs, &registry, 0),
        vec![31],
        "the primary is held out of the disable pass"
    );
}

/// A layer that can take the pinned slot needs no anchor: it will be assigned
/// there, and an assigned plane is not disabled.
#[test]
fn a_layer_matching_the_pin_makes_the_anchor_unnecessary() {
    let registry = drmkit_planes::PlaneRegistry::from_capabilities(vec![
        reservation_plane(31, drmkit_planes::PlaneType::Primary, Some((2, 2))),
        reservation_plane(32, drmkit_planes::PlaneType::Overlay, Some((3, 9))),
    ]);
    let layers = [reservation_layer(Some(2)), reservation_layer(Some(3))];
    let refs: Vec<drmkit_planes::LayerRef<'_>> = layers
        .iter()
        .enumerate()
        .map(|(i, layer)| drmkit_planes::LayerRef {
            id: drmkit_planes::LayerId(i as u64 + 1),
            layer,
        })
        .collect();

    assert!(crate::scene::canvas_reservation(true, &refs, &registry, 0).is_empty());
}

/// So does a layer with no zpos at all: it is free to land on the pin, and
/// `plane_score`'s primary bonus is what puts it there.
#[test]
fn a_layer_with_no_zpos_makes_the_anchor_unnecessary() {
    let registry = drmkit_planes::PlaneRegistry::from_capabilities(vec![
        reservation_plane(31, drmkit_planes::PlaneType::Primary, Some((2, 2))),
        reservation_plane(32, drmkit_planes::PlaneType::Overlay, Some((3, 9))),
    ]);
    let layers = [reservation_layer(None)];
    let refs: Vec<drmkit_planes::LayerRef<'_>> = layers
        .iter()
        .enumerate()
        .map(|(i, layer)| drmkit_planes::LayerRef {
            id: drmkit_planes::LayerId(i as u64 + 1),
            layer,
        })
        .collect();

    assert!(crate::scene::canvas_reservation(true, &refs, &registry, 0).is_empty());
}

/// A primary with no zpos property is not pinned, so there is nothing to
/// anchor. This is vkms, which is why no lane can reach the case above.
#[test]
fn an_unpinned_primary_is_not_anchored() {
    let registry = drmkit_planes::PlaneRegistry::from_capabilities(vec![
        reservation_plane(31, drmkit_planes::PlaneType::Primary, None),
        reservation_plane(32, drmkit_planes::PlaneType::Overlay, None),
    ]);
    let layers = [reservation_layer(Some(3))];
    let refs: Vec<drmkit_planes::LayerRef<'_>> = layers
        .iter()
        .enumerate()
        .map(|(i, layer)| drmkit_planes::LayerRef {
            id: drmkit_planes::LayerId(i as u64 + 1),
            layer,
        })
        .collect();

    assert!(crate::scene::canvas_reservation(true, &refs, &registry, 0).is_empty());
}

/// The overflow trigger still works, and still takes the last candidate so the
/// layers below the canvas keep the lower-indexed planes.
#[test]
fn overflow_still_reserves_the_last_candidate() {
    let registry = drmkit_planes::PlaneRegistry::from_capabilities(vec![
        reservation_plane(31, drmkit_planes::PlaneType::Primary, None),
        reservation_plane(32, drmkit_planes::PlaneType::Overlay, None),
    ]);
    let layers: Vec<drmkit_planes::Layer> = (0..3).map(|_| reservation_layer(None)).collect();
    let refs: Vec<drmkit_planes::LayerRef<'_>> = layers
        .iter()
        .enumerate()
        .map(|(i, layer)| drmkit_planes::LayerRef {
            id: drmkit_planes::LayerId(i as u64 + 1),
            layer,
        })
        .collect();

    assert_eq!(
        crate::scene::canvas_reservation(true, &refs, &registry, 0),
        vec![32]
    );
}

/// The acquire-fence close discipline, at the level the scene works at.
///
/// `drmkit-sync` covers the fence itself -- that `import` dups, that a move
/// carries the descriptor, that repeated import and drop leaks nothing. What
/// is left is the composition: an `AcquiredBuffer` carrying one, moved into a
/// release ring and dropped, once per layer per frame.
mod acquire_fence {
    use std::os::fd::{AsFd as _, OwnedFd};

    use drmkit_sync::SyncFence;

    use crate::AcquiredBuffer;

    /// An eventfd with a count already written: readable, so `poll` reports it
    /// immediately. The same stand-in for a signalled `sync_file` that
    /// `drmkit-sync` uses, and cheaper than a pipe -- one descriptor rather
    /// than two.
    fn signalled() -> OwnedFd {
        rustix::event::eventfd(1, rustix::event::EventfdFlags::CLOEXEC).expect("eventfd")
    }

    fn open_fd_count() -> usize {
        std::fs::read_dir("/proc/self/fd")
            .expect("/proc/self/fd")
            .count()
    }

    #[test]
    fn a_buffer_with_no_fence_says_so() {
        // The common case: a source that produces ready buffers synchronously
        // leaves this unset, and the scene must not wait on nothing.
        let acquired = AcquiredBuffer::default();
        assert!(acquired.acquire_fence.is_none());
    }

    #[test]
    fn churning_fenced_buffers_does_not_leak_descriptors() {
        // Once per layer per frame, so a leak of one descriptor per acquire
        // reaches EMFILE in seconds at 60 Hz. `OwnedFd` makes the double-close
        // half of this unrepresentable; the leak half is still reachable
        // through a `forget` or a descriptor stashed somewhere and never
        // dropped, which is what this measures.
        let source = signalled();

        let churn = |times: usize| {
            for _ in 0..times {
                let fence = SyncFence::import(source.as_fd()).expect("import");
                let acquired = AcquiredBuffer {
                    fb_id: 1,
                    acquire_fence: Some(fence),
                    ..AcquiredBuffer::default()
                };
                // Moved as the scene moves it into its release ring, then
                // dropped at the end of the frame.
                let moved = acquired;
                assert!(moved.acquire_fence.is_some());
                drop(moved);
            }
        };

        churn(16);
        let after_few = open_fd_count();
        churn(240);
        let growth = open_fd_count().saturating_sub(after_few);

        assert!(
            growth < 16,
            "240 further acquire/drop cycles grew the fd table by {growth}; \
             a per-acquire leak would grow it by 240"
        );
    }

    #[test]
    fn a_fence_carried_through_a_move_is_still_waitable() {
        // The move must carry the descriptor rather than a copy of a number
        // that something else has since closed.
        let source = signalled();
        let acquired = AcquiredBuffer {
            fb_id: 1,
            acquire_fence: Some(SyncFence::import(source.as_fd()).expect("import")),
            ..AcquiredBuffer::default()
        };
        let moved = acquired;

        let fence = moved.acquire_fence.expect("the move kept it");
        fence
            .wait(std::time::Duration::from_millis(200))
            .expect("an already-signalled fence waits successfully");
    }
}

/// The scene's small surfaces, ported from `test_scene.cpp`.
///
/// The file is forty-one cases against an API that differs in shape here --
/// upstream has per-field `set_*_if_changed` setters where this has one
/// `set_display`, and constructors that take a device where this takes a CRTC
/// id. What follows is the behaviour that exists on both sides; the parity row
/// names what does not and why.
mod scene_surfaces {
    use std::collections::HashMap;

    use drmkit_planes::{ContentType, Rect};

    use crate::{DisplayParams, LayerHandle, LayerScene, Placement};

    #[test]
    fn a_default_handle_is_the_invalid_sentinel() {
        // Slot 0 is reserved for exactly this, so a handle nobody assigned
        // cannot address the first layer.
        assert!(!LayerHandle::default().is_valid());
    }

    #[test]
    fn a_handle_distinguishes_slot_from_generation() {
        // Both halves have to participate in equality and in the hash. A
        // handle comparing on slot alone would let a stale one match the
        // layer that took its place, which is the whole hazard the generation
        // exists to close.
        let mut scene = LayerScene::new(1);
        let first = scene.add_layer(Box::new(super::MinimalSource));
        scene.remove_layer(first);
        let second = scene.add_layer(Box::new(super::MinimalSource));

        assert_ne!(first, second, "the recycled slot is a different handle");
        assert_ne!(
            first.layer_id(),
            second.layer_id(),
            "and a different identity"
        );

        // Usable as a map key, and the two do not collide there either.
        let mut by_handle: HashMap<LayerHandle, &str> = HashMap::new();
        by_handle.insert(first, "gone");
        by_handle.insert(second, "live");
        assert_eq!(by_handle.len(), 2);
        assert_eq!(by_handle.get(&second), Some(&"live"));
    }

    #[test]
    fn a_stale_handle_resolves_to_nothing() {
        let mut scene = LayerScene::new(1);
        let first = scene.add_layer(Box::new(super::MinimalSource));
        scene.remove_layer(first);
        let second = scene.add_layer(Box::new(super::MinimalSource));

        assert!(scene.layer(first).is_none(), "the removed layer is gone");
        assert!(scene.layer(second).is_some());
    }

    #[test]
    fn geometry_does_not_flag_a_layer_for_reallocation() {
        // Moving or resizing a layer changes what is written to the plane it
        // already has. Dropping the warm start for it would cost a search
        // every frame something moves, which is every frame.
        let mut scene = LayerScene::new(1);
        let handle = scene.add_layer(Box::new(super::MinimalSource));
        let layer = scene.layer_mut(handle).expect("live");
        assert!(!layer.hints_dirty());

        layer.set_display(DisplayParams {
            dst_rect: Rect {
                x: 10,
                y: 20,
                w: 64,
                h: 64,
            },
            ..DisplayParams::default()
        });
        assert!(
            !layer.hints_dirty(),
            "geometry must not drop the warm start"
        );
    }

    #[test]
    fn a_hint_that_changes_scoring_does_flag_it() {
        // Content type, refresh hint and app priority all feed plane scoring,
        // so the layer has to be free to move to a plane its new hint
        // prefers -- which it cannot do from a warm start.
        let mut scene = LayerScene::new(1);
        let handle = scene.add_layer(Box::new(super::MinimalSource));

        // A fresh layer per case: the flag is cleared by a commit, and there
        // is deliberately no public way to clear it otherwise.
        for set in [
            &(|l: &mut crate::SceneLayer| l.set_content_type(ContentType::Video))
                as &dyn Fn(&mut crate::SceneLayer),
            &|l: &mut crate::SceneLayer| l.set_update_hint(24),
            &|l: &mut crate::SceneLayer| l.set_app_priority(3),
        ] {
            let fresh = scene.add_layer(Box::new(super::MinimalSource));
            let layer = scene.layer_mut(fresh).expect("live");
            assert!(!layer.hints_dirty(), "starts clean");
            set(layer);
            assert!(layer.hints_dirty());
        }
        let _ = handle;
    }

    #[test]
    fn the_hint_accessors_read_back_what_was_set() {
        let mut scene = LayerScene::new(1);
        let handle = scene.add_layer(Box::new(super::MinimalSource));
        let layer = scene.layer_mut(handle).expect("live");

        assert_eq!(layer.app_priority(), 0, "defaults to none");
        assert_eq!(layer.update_hint_hz(), 0);
        assert_eq!(layer.content_type(), ContentType::default());

        layer.set_app_priority(7);
        layer.set_update_hint(48);
        layer.set_content_type(ContentType::Video);

        assert_eq!(layer.app_priority(), 7);
        assert_eq!(layer.update_hint_hz(), 48);
        assert_eq!(layer.content_type(), ContentType::Video);
    }

    #[test]
    fn a_placement_defaults_to_unassigned_with_no_plane() {
        // The safe default: a layer nothing recorded an outcome for has not
        // reached the screen, and claiming a plane it does not have would be
        // worse than claiming none.
        let placement = crate::LayerPlacement {
            layer: drmkit_planes::LayerId(1),
            ..crate::LayerPlacement::default()
        };
        assert_eq!(placement.placement, Placement::Unassigned);
        assert_eq!(placement.plane_id, None);
        assert_eq!(placement.plane_rotation_bits, 0);
    }

    #[test]
    fn a_fresh_report_counts_nothing() {
        let report = crate::CommitReport::default();
        assert_eq!(report.layers_total, 0);
        assert_eq!(report.layers_assigned, 0);
        assert_eq!(report.layers_composited, 0);
        assert_eq!(report.test_commits_issued, 0);
        assert!(report.placements.is_empty());
        assert!(
            report.accounting_balances(),
            "zero of everything still has to add up"
        );
    }

    #[test]
    fn the_scene_remembers_which_crtc_it_drives() {
        assert_eq!(LayerScene::new(42).crtc_id(), 42);
    }
}
