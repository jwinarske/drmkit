// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Deferred buffer release.
//!
//! Ports the acquisition ring from `src/scene/layer_scene.cpp`. This is where
//! **invariants 1, 2 and 5** live.

use drmkit_planes::LayerId;
use drmkit_sync::SyncFence;

use crate::source::AcquiredBuffer;

/// One buffer held on behalf of a layer.
#[derive(Debug)]
pub struct Acquisition {
    /// Which layer acquired it.
    pub layer: LayerId,
    /// The buffer itself.
    pub buffer: AcquiredBuffer,
    /// The `OUT_FENCE` of the commit that displaced it, stamped on when it
    /// leaves the screen and handed back with the buffer.
    ///
    /// `None` when the CRTC has no `OUT_FENCE_PTR` or no source opted in.
    pub release_fence: Option<SyncFence>,
}

impl Acquisition {
    /// Hold `buffer` on behalf of `layer`.
    #[must_use]
    pub const fn new(layer: LayerId, buffer: AcquiredBuffer) -> Self {
        Self {
            layer,
            buffer,
            release_fence: None,
        }
    }
}

/// Why a set of buffers is being handed back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseReason {
    /// They stopped being scanned out: a later commit's flip completed.
    ///
    /// The ordinary path, and the one invariant 1 governs.
    Retired,
    /// The commit that would have shown them was rejected.
    ///
    /// Invariant 2: no flip is in flight to gate them on, so they go back at
    /// once and the producer can reuse them immediately.
    CommitFailed,
    /// A test commit, which applies nothing to hardware.
    TestOnly,
    /// The scene is being torn down, paused, or rebound.
    Drained,
}

/// The buffers to hand back, and why.
#[derive(Debug)]
pub struct Released {
    /// The buffers, oldest first.
    pub acquisitions: Vec<Acquisition>,
    /// Why they are being returned.
    pub reason: ReleaseReason,
}

impl Released {
    /// Whether anything is being returned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.acquisitions.is_empty()
    }

    /// How many buffers are being returned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.acquisitions.len()
    }
}

/// The two-deep ring of in-flight acquisitions.
///
/// # Invariant 1: release after the flip, not after the ioctl
///
/// A buffer goes back to its source only once the page flip that **stops**
/// scanning it out has completed — two commits deep, never when `commit`'s
/// ioctl returns. Releasing at ioctl return would let a producer overwrite a
/// buffer the display engine is still reading, which tears on hardware whose
/// producer driver attaches no reservation fence.
///
/// Three generations are tracked:
///
/// | Generation | Frame | State |
/// |---|---|---|
/// | `current` | N | just committed, about to be scanned out |
/// | `previous` | N-1 | on screen; the flip for N displaces it |
/// | `two_ago` | N-2 | off screen — safe to return |
///
/// A successful commit returns `two_ago`, then rotates. That is the whole
/// mechanism.
///
/// # Why this is not the scene's own field
///
/// The queue never calls a source. It *yields* the buffers to hand back and
/// lets the caller route each to the right source, because a source is owned
/// by its layer and the queue has no business borrowing the layer table. It
/// also means the entire contract is testable with no device, no source, and
/// no kernel — which is what these tests do.
#[derive(Debug, Default)]
pub struct ReleaseQueue {
    /// Frame N-1: on screen now.
    previous: Vec<Acquisition>,
    /// Frame N-2: off screen, returned by the next successful commit.
    two_ago: Vec<Acquisition>,
}

impl ReleaseQueue {
    /// An empty queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            previous: Vec::new(),
            two_ago: Vec::new(),
        }
    }

    /// How many buffers are held in flight.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.previous.len() + self.two_ago.len()
    }

    /// Whether any buffer is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.previous.is_empty() && self.two_ago.is_empty()
    }

    /// Record a **successful real commit** and return the buffers it retired.
    ///
    /// **Invariant 1.** The returned buffers are frame N-2's: the flip just
    /// armed displaces frame N-1, so N-2 is what has finished scanning out.
    /// Frame N's buffers stay held.
    ///
    /// `release_fence` is this commit's `OUT_FENCE`. It signals when the
    /// buffers currently on screen — frame N-1 — leave it, so it is stamped
    /// onto those and rides down to be handed back next rotation. A source that
    /// wants it can then wait GPU-side instead of blocking the CPU until the
    /// later release edge.
    ///
    /// `wants_fence` decides per layer whether to stamp, so a scene with no
    /// interested source pays nothing.
    pub fn commit_succeeded<F>(
        &mut self,
        current: Vec<Acquisition>,
        release_fence: Option<&SyncFence>,
        mut wants_fence: F,
    ) -> Released
    where
        F: FnMut(LayerId) -> bool,
    {
        if let Some(fence) = release_fence {
            for acquisition in &mut self.previous {
                if wants_fence(acquisition.layer)
                    && let Some(fd) = fence.as_fd()
                    && let Ok(duplicate) = SyncFence::import(fd)
                {
                    acquisition.release_fence = Some(duplicate);
                }
            }
        }

        // Frame N-2 has finished scanning out.
        let retired = std::mem::take(&mut self.two_ago);
        // Rotate: N-1 becomes N-2, N becomes N-1.
        self.two_ago = std::mem::take(&mut self.previous);
        self.previous = current;

        Released {
            acquisitions: retired,
            reason: ReleaseReason::Retired,
        }
    }

    /// Record a **failed** commit and return this frame's buffers at once.
    ///
    /// **Invariant 2.** No flip is in flight to gate them on, so deferring
    /// would just stall the source's ring. Self-healing producer rings depend
    /// on reusing their buffers immediately after a rejection; the dropped
    /// frame heals on the next one.
    ///
    /// The in-flight generations are **not** disturbed: the previous commit's
    /// buffers are still on screen, and a failed commit did not displace them.
    pub fn commit_failed(&mut self, current: Vec<Acquisition>) -> Released {
        Released {
            acquisitions: current,
            reason: ReleaseReason::CommitFailed,
        }
    }

    /// Record a **test** commit and return its buffers at once.
    ///
    /// A `TEST_ONLY` applies nothing to hardware, so nothing in the scanout
    /// pipeline references these buffers and deferring would only stall the
    /// ring. Distinguished from [`commit_failed`](Self::commit_failed) because
    /// the caller reports them differently, not because the buffers are handled
    /// differently.
    pub fn test_committed(&mut self, current: Vec<Acquisition>) -> Released {
        Released {
            acquisitions: current,
            reason: ReleaseReason::TestOnly,
        }
    }

    /// Return **everything** held, oldest first.
    ///
    /// **Invariant 5's counterpart.** For teardown, session pause, and rebind —
    /// paths where no flip is in flight, so holding buffers back from their
    /// sources would simply leak them.
    ///
    /// This does not wait on the kernel and must not: the scene's drop stays
    /// non-blocking, and landing an armed flip first is the caller's job. See
    /// [`ScenePendingFlip`](crate::ScenePendingFlip).
    pub fn drain(&mut self) -> Released {
        let mut acquisitions = std::mem::take(&mut self.two_ago);
        acquisitions.append(&mut self.previous);
        Released {
            acquisitions,
            reason: ReleaseReason::Drained,
        }
    }
}

/// Tracks whether a commit armed a page-flip event that has not been
/// dispatched.
///
/// # Invariant 5: teardown does not wait on the kernel
///
/// Dropping a scene releases its buffers and framebuffers **without** waiting.
/// If the last real commit armed `DRM_MODE_PAGE_FLIP_EVENT` and that event has
/// not been dispatched, the flip still references a framebuffer the drop tears
/// down — the RmFB-on-in-flight-FB hazard. The caller lands it first, by
/// draining or by dispatching the event itself.
///
/// The C++ can only document that. Here the scene's `Drop` consults this and
/// warns when a flip is still armed, which is observability the C++ cannot
/// cheaply add — it turns a hazard that shows up as a rare tear into something
/// loud in development.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScenePendingFlip {
    armed: bool,
}

impl ScenePendingFlip {
    /// Nothing armed.
    #[must_use]
    pub const fn new() -> Self {
        Self { armed: false }
    }

    /// Record that a commit armed a page-flip event.
    pub const fn arm(&mut self) {
        self.armed = true;
    }

    /// Record that the event was dispatched.
    pub const fn landed(&mut self) {
        self.armed = false;
    }

    /// Whether a flip is still outstanding.
    ///
    /// Dropping the scene while this is true is the hazard invariant 5
    /// describes.
    #[must_use]
    pub const fn is_armed(&self) -> bool {
        self.armed
    }
}
