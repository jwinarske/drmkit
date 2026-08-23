// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What one scene commit did.
//!
//! Port of `src/scene/commit_report.hpp`.

use drmkit_planes::LayerId;

/// How a layer reached scanout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Placement {
    /// On its own hardware plane.
    AssignedToPlane,
    /// Composited into the canvas plane.
    Composited,
    /// Dropped this frame — no plane available, no way to composite it, or the
    /// canvas could not be allocated.
    #[default]
    Unassigned,
}

/// One layer's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerPlacement {
    /// Which layer.
    pub layer: LayerId,
    /// How it reached scanout.
    pub placement: Placement,
    /// The plane its content scanned out on, or `None` when unassigned.
    ///
    /// For a composited layer this is the **canvas** plane, not a plane of its
    /// own.
    pub plane_id: Option<u32>,
    /// The assigned plane's supported rotate/reflect mask, for an
    /// [`AssignedToPlane`](Placement::AssignedToPlane) layer; 0 otherwise.
    ///
    /// Lets a producer decide whether its requested angle can land on the plane
    /// it was given or needs pre-rotation.
    pub plane_rotation_bits: u64,
}

/// A snapshot of a single commit.
///
/// Every field is a count that starts at zero. `#[non_exhaustive]` because the
/// C++ has grown counters additively — `layers_skipped_no_frame`,
/// `fb_delta_fast_path`, the fence counters — and keeping that freedom means a
/// new counter is not a breaking change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CommitReport {
    /// Layers in the scene at commit time.
    ///
    /// # Accounting invariant
    ///
    /// ```text
    /// layers_total == layers_assigned
    ///               + layers_composited
    ///               + layers_unassigned
    ///               + layers_skipped_no_frame
    /// ```
    ///
    /// Checked by [`accounting_balances`](Self::accounting_balances). This is
    /// why a layer with no format is never placed: counting it as both assigned
    /// and skipped would break this.
    pub layers_total: usize,

    /// Layers the allocator put directly on a hardware plane.
    pub layers_assigned: usize,

    /// Layers rescued by composition into the canvas. They do reach hardware —
    /// via the canvas plane rather than one of their own.
    pub layers_composited: usize,

    /// Layers neither placed nor composited: dropped this frame.
    pub layers_unassigned: usize,

    /// Layers whose source returned [`WouldBlock`](crate::SourceError::WouldBlock)
    /// and were skipped.
    ///
    /// Flow control, not failure — a live source with nothing to contribute
    /// this vblank. Graphable as a per-layer frame-stall signal.
    pub layers_skipped_no_frame: usize,

    /// Composition buckets emitted. 0 or 1 with a single full-screen canvas.
    pub composition_buckets: usize,

    /// Layers that asked to be pinned to a plane but could not be.
    ///
    /// The pin target was not on this CRTC, did not support the format, or was
    /// already claimed. **The layer is not dropped** — it falls back to normal
    /// allocation and is still counted in one of the placement tallies, so this
    /// does not affect the accounting invariant. Nonzero is a loud signal that
    /// a caller's deterministic-plane assumption was violated, as opposed to a
    /// silent drop.
    pub pin_requests_unhonored: usize,

    /// Properties enqueued on the atomic request — plane, CRTC, and connector
    /// state together.
    ///
    /// A regression signal for property minimization: it should drop
    /// meaningfully on a commit whose layers did not change.
    pub properties_written: usize,

    /// Subset of `properties_written` that are `FB_ID` attachments.
    pub fbs_attached: usize,

    /// Subset of `properties_written`: layers that got an `FB_DAMAGE_CLIPS`
    /// blob.
    ///
    /// Zero on a full-frame commit — no damage reported, over budget,
    /// degenerate clips, or a plane without the property. Tells a
    /// bandwidth-sensitive consumer whether partial updates actually reach the
    /// kernel.
    pub damaged_layers: usize,

    /// Layers whose acquire fence was handed to the kernel as `IN_FENCE_FD` —
    /// the real explicit-sync path, with no CPU stall.
    ///
    /// The honest signal that explicit sync is live on this driver: a source
    /// may always produce a fence and the plane still not accept it, which
    /// shows up in [`in_fence_cpu_waits`](Self::in_fence_cpu_waits) instead.
    pub in_fences_armed: usize,

    /// Layers that carried a fence the plane could not take, so the scene
    /// blocked on a CPU wait before committing.
    ///
    /// Real commits only; a test never waits. Both counters zero means no
    /// source produced a fence at all.
    pub in_fence_cpu_waits: usize,

    /// Internal test commits the allocator issued while searching.
    ///
    /// A cold start can spend the whole budget; a warm start is 1; the FB-only
    /// fast path is 0.
    pub test_commits_issued: usize,

    /// Whether this commit took the FB-only fast path — only content changed on
    /// already-placed layers, so the cached assignment was reused and the
    /// `TEST_ONLY` skipped entirely.
    ///
    /// **Invariant 4's observable signal.** Implies
    /// `test_commits_issued == 0`, which a steady-state consumer asserts to
    /// confirm it is paying one atomic commit per frame rather than two.
    pub fb_delta_fast_path: bool,

    /// Per-layer outcomes.
    pub placements: Vec<LayerPlacement>,
}

impl CommitReport {
    /// Whether the layer tallies add up to [`layers_total`](Self::layers_total).
    ///
    /// The C++ states this as a comment on the field. Making it checkable means
    /// the scene's own tests can assert it every commit, which is how a layer
    /// counted twice — or not at all — gets caught at the point it happens.
    #[must_use]
    pub const fn accounting_balances(&self) -> bool {
        self.layers_total
            == self.layers_assigned
                + self.layers_composited
                + self.layers_unassigned
                + self.layers_skipped_no_frame
    }

    /// Whether the fast path's implied contract holds: taking it means issuing
    /// no test commits.
    #[must_use]
    pub const fn fast_path_consistent(&self) -> bool {
        !self.fb_delta_fast_path || self.test_commits_issued == 0
    }
}
