// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What a commit does to scene state once the kernel has answered.
//!
//! Ports the `finalize_frame` reconciliation from `src/scene/layer_scene.cpp`.
//!
//! # Why this is a separate type
//!
//! The C++ `finalize_frame` takes the kernel's result as a parameter, so the
//! scene's reaction to success, rejection, and lost master is separable from
//! actually talking to a device. Keeping that shape means every rule below —
//! the release timing, the suspend, the cache invalidation — is testable by
//! passing a different result, with no kernel involved.

use drmkit_planes::LayerId;

use crate::release::{Acquisition, ReleaseQueue, Released, ScenePendingFlip};
use crate::report::CommitReport;

/// What the kernel said about a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelResult {
    /// Applied. For a real commit this means a flip is on its way.
    Ok,
    /// Rejected because the caller is not DRM master.
    ///
    /// The **only** failure that suspends the scene.
    NotMaster,
    /// Rejected for any other reason.
    ///
    /// The frame is dropped and heals on the next one. The scene keeps running.
    Rejected,
}

/// What one commit was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitKind {
    /// A real commit. May arm a page-flip event.
    Real {
        /// Whether `DRM_MODE_PAGE_FLIP_EVENT` was armed.
        arms_flip: bool,
    },
    /// A `TEST_ONLY` commit, which applies nothing to hardware.
    Test,
}

/// What the scene must do after a commit.
#[derive(Debug)]
pub struct FrameOutcome {
    /// Buffers to hand back to their sources, and why.
    pub released: Released,
    /// The report for this commit.
    pub report: CommitReport,
    /// Whether the scene is now suspended.
    pub suspended: bool,
    /// Whether the allocator's cached assignment must be dropped.
    ///
    /// Set when the kernel rejected a frame whose `TEST_ONLY` was skipped —
    /// invariant 4's safety net.
    pub invalidate_allocation: bool,
}

/// Owns the state a commit reconciles: the release ring, the armed-flip
/// tripwire, and whether the scene is suspended.
#[derive(Debug, Default)]
pub struct FrameLifecycle {
    releases: ReleaseQueue,
    pending_flip: ScenePendingFlip,
    suspended: bool,
    /// Whether the fast-path rejection warning has already been emitted.
    warned_fast_path_reject: bool,
}

impl FrameLifecycle {
    /// A fresh lifecycle with nothing in flight.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            releases: ReleaseQueue::new(),
            pending_flip: ScenePendingFlip::new(),
            suspended: false,
            warned_fast_path_reject: false,
        }
    }

    /// Whether the scene is suspended and short-circuiting commits.
    #[must_use]
    pub const fn is_suspended(&self) -> bool {
        self.suspended
    }

    /// Whether a page-flip event is armed and undispatched.
    ///
    /// **Invariant 5.** Dropping the scene while this is true tears down a
    /// framebuffer the flip still references.
    #[must_use]
    pub const fn has_pending_flip(&self) -> bool {
        self.pending_flip.is_armed()
    }

    /// How many buffers are held in flight.
    #[must_use]
    pub fn buffers_in_flight(&self) -> usize {
        self.releases.in_flight()
    }

    /// Record that the armed page-flip event was dispatched.
    pub const fn flip_landed(&mut self) {
        self.pending_flip.landed();
    }

    /// Lift a suspension after the session resumes.
    pub const fn resume(&mut self) {
        self.suspended = false;
    }

    /// Reconcile scene state with the kernel's answer.
    ///
    /// # The rules, and why each is where it is
    ///
    /// - **Success on a real commit** rotates the release ring and returns the
    ///   buffers two commits deep — invariant 1.
    /// - **Any failure** returns this frame's buffers at once, because no flip
    ///   is in flight to gate them on — invariant 2.
    /// - **Only `NotMaster` suspends.** A non-`EACCES` rejection drops the
    ///   frame and heals on the next one; suspending on it would take a
    ///   recoverable stumble and stop the scene. The C++ states this as an
    ///   external contract, and self-healing producer rings depend on it.
    /// - **A rejected fast-path frame invalidates the cached allocation.**
    ///   Invariant 4's safety net: the kernel refused a frame whose `TEST_ONLY`
    ///   was skipped, so the cache is no longer trustworthy. The frame is
    ///   dropped but heals — the failure mode is never corruption. A recurring
    ///   rejection here means a property was misclassified as content, which is
    ///   why it is worth warning about once.
    /// - **A test commit** returns its buffers at once and changes nothing
    ///   else: it applied nothing to hardware.
    pub fn finalize(
        &mut self,
        kind: CommitKind,
        result: KernelResult,
        acquisitions: Vec<Acquisition>,
        mut report: CommitReport,
        release_fence: Option<&drmkit_sync::SyncFence>,
        wants_fence: impl FnMut(LayerId) -> bool,
    ) -> FrameOutcome {
        if let CommitKind::Test = kind {
            return FrameOutcome {
                released: self.releases.test_committed(acquisitions),
                report,
                suspended: self.suspended,
                invalidate_allocation: false,
            };
        }

        match result {
            KernelResult::Ok => {
                if matches!(kind, CommitKind::Real { arms_flip: true }) {
                    self.pending_flip.arm();
                }
                let released =
                    self.releases
                        .commit_succeeded(acquisitions, release_fence, wants_fence);
                FrameOutcome {
                    released,
                    report,
                    suspended: self.suspended,
                    invalidate_allocation: false,
                }
            }
            KernelResult::NotMaster => {
                self.suspended = true;
                FrameOutcome {
                    released: self.releases.commit_failed(acquisitions),
                    report,
                    suspended: true,
                    invalidate_allocation: false,
                }
            }
            KernelResult::Rejected => {
                // Invariant 4's safety net. Only meaningful when the TEST was
                // skipped -- an ordinary rejection after a real TEST says
                // nothing new about the cache.
                let invalidate = report.fb_delta_fast_path;
                if invalidate {
                    self.warned_fast_path_reject = true;
                    // The frame is dropped, not corrupted: the cache is
                    // dropped, the next frame re-searches, and the report no
                    // longer claims a fast path that did not hold.
                    report.fb_delta_fast_path = false;
                }
                FrameOutcome {
                    released: self.releases.commit_failed(acquisitions),
                    report,
                    suspended: self.suspended,
                    invalidate_allocation: invalidate,
                }
            }
        }
    }

    /// Whether the fast-path rejection has been warned about already, so the
    /// caller logs once rather than every frame.
    #[must_use]
    pub const fn has_warned_fast_path_reject(&self) -> bool {
        self.warned_fast_path_reject
    }

    /// Hand back every held buffer, for teardown, pause, or rebind.
    ///
    /// **Invariant 5.** Does not wait on the kernel. Landing an armed flip
    /// first is the caller's job — [`has_pending_flip`](Self::has_pending_flip)
    /// says whether one is outstanding.
    pub fn drain(&mut self) -> Released {
        self.releases.drain()
    }
}

/// Accounting for one build pass's acquire loop.
///
/// Separated so the EAGAIN rule is testable on its own: a source with nothing
/// to contribute is **skipped**, counted, and retried next frame — it is not a
/// failure, and it must not abort the commit.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AcquireTally {
    /// Layers that produced a buffer.
    pub acquired: usize,
    /// Layers whose source reported [`WouldBlock`](crate::SourceError::WouldBlock).
    pub skipped_no_frame: usize,
}

impl AcquireTally {
    /// Fold one source's outcome into the tally.
    ///
    /// Returns whether the layer contributed a buffer.
    pub const fn record(&mut self, acquired: bool) -> bool {
        if acquired {
            self.acquired += 1;
        } else {
            self.skipped_no_frame += 1;
        }
        acquired
    }

    /// How many layers the loop considered.
    #[must_use]
    pub const fn considered(&self) -> usize {
        self.acquired + self.skipped_no_frame
    }
}
