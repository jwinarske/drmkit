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
