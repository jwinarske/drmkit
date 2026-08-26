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
///
/// The default is the safe one: unassigned, on no plane. A placement nothing
/// filled in describes a layer that did not reach the screen, which is a
/// smaller claim than naming a plane it does not have.
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

impl Default for LayerPlacement {
    /// Unassigned, on no plane, for no layer.
    ///
    /// Written by hand rather than derived so `LayerId` does not have to gain
    /// a `Default` it has no use for elsewhere -- an id is something the scene
    /// hands out, and one a caller could conjure would be a way to address a
    /// layer that does not exist.
    fn default() -> Self {
        Self {
            layer: LayerId(0),
            placement: Placement::Unassigned,
            plane_id: None,
            plane_rotation_bits: 0,
        }
    }
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

    /// Whether the per-frame test-commit budget, not the hardware, is what
    /// stopped more layers reaching planes.
    ///
    /// When this is set, some of `layers_composited` is a capacity decision
    /// rather than a plane shortage: there were compatible planes free and the
    /// allocator ran out of permitted round trips to the kernel before it
    /// could offer them. Composited layers still reach the screen, so this is
    /// not an error -- but a caller tuning a scene needs to tell "this device
    /// has no more planes" from "raise the budget", and `layers_composited`
    /// alone cannot. Raise it with `Allocator::set_max_test_commits`.
    pub budget_exhausted: bool,

    /// Per-layer outcomes.
    pub placements: Vec<LayerPlacement>,
}

impl CommitReport {
    /// What became of one layer this commit.
    ///
    /// `None` when the layer was not in the frame at all -- it was never
    /// added, it was removed, or the handle is stale. A stale one cannot
    /// match by accident: [`LayerHandle::layer_id`](crate::LayerHandle::layer_id)
    /// packs the slot's generation, so a recycled slot yields a different id
    /// and a handle to the layer that used to live there resolves to nothing.
    #[must_use]
    pub fn placement_of(&self, layer: impl Into<LayerId>) -> Option<&LayerPlacement> {
        let id = layer.into();
        self.placements.iter().find(|entry| entry.layer == id)
    }

    /// Whether the layer went into the canvas rather than onto a plane of its
    /// own.
    ///
    /// False for a layer that was dropped as well as for one that got a
    /// plane: composited means it reached the screen the slow way, and
    /// unassigned means it did not reach the screen at all. A caller
    /// throttling its repaints on this distinction wants them separated.
    #[must_use]
    pub fn was_composited(&self, layer: impl Into<LayerId>) -> bool {
        self.placement_of(layer)
            .is_some_and(|entry| entry.placement == Placement::Composited)
    }

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

#[cfg(test)]
mod tests {
    use super::{CommitReport, LayerPlacement, Placement};
    use drmkit_planes::LayerId;

    /// A report with one layer of each outcome.
    fn report() -> CommitReport {
        CommitReport {
            placements: vec![
                LayerPlacement {
                    layer: LayerId(1),
                    placement: Placement::AssignedToPlane,
                    plane_id: Some(31),
                    plane_rotation_bits: 0,
                },
                LayerPlacement {
                    layer: LayerId(2),
                    placement: Placement::Composited,
                    plane_id: Some(42),
                    plane_rotation_bits: 0,
                },
                LayerPlacement {
                    layer: LayerId(3),
                    placement: Placement::Unassigned,
                    plane_id: None,
                    plane_rotation_bits: 0,
                },
            ],
            ..CommitReport::default()
        }
    }

    #[test]
    fn each_outcome_is_found_with_the_plane_it_landed_on() {
        let report = report();

        let assigned = report.placement_of(LayerId(1)).expect("assigned");
        assert_eq!(assigned.placement, Placement::AssignedToPlane);
        assert_eq!(assigned.plane_id, Some(31));

        // A composited layer names the *canvas* plane, not one of its own --
        // a caller reading this as "the layer got plane 42" would think it had
        // a plane it does not have.
        let composited = report.placement_of(LayerId(2)).expect("composited");
        assert_eq!(composited.placement, Placement::Composited);
        assert_eq!(composited.plane_id, Some(42));

        let dropped = report.placement_of(LayerId(3)).expect("unassigned");
        assert_eq!(dropped.placement, Placement::Unassigned);
        assert_eq!(dropped.plane_id, None, "it reached no plane at all");
    }

    #[test]
    fn a_layer_that_was_not_in_the_frame_has_no_placement() {
        assert!(report().placement_of(LayerId(4)).is_none());
        assert!(CommitReport::default().placement_of(LayerId(1)).is_none());
    }

    #[test]
    fn a_recycled_slot_does_not_answer_for_the_layer_that_left_it() {
        // The identity a handle resolves to packs the slot's generation, so
        // the layer that took a freed slot has a different id and a stale
        // handle finds nothing. Without that, a caller holding a handle to a
        // removed layer would read the new occupant's outcome as its own.
        let slot_1_generation_1 = LayerId((1 << 32) | 1);
        let slot_1_generation_2 = LayerId((1 << 32) | 2);
        assert_ne!(slot_1_generation_1, slot_1_generation_2);

        let report = CommitReport {
            placements: vec![LayerPlacement {
                layer: slot_1_generation_2,
                placement: Placement::AssignedToPlane,
                plane_id: Some(31),
                plane_rotation_bits: 0,
            }],
            ..CommitReport::default()
        };
        assert!(report.placement_of(slot_1_generation_2).is_some());
        assert!(report.placement_of(slot_1_generation_1).is_none());
    }

    #[test]
    fn only_a_composited_layer_was_composited() {
        let report = report();
        assert!(!report.was_composited(LayerId(1)), "it got a plane");
        assert!(report.was_composited(LayerId(2)));
        // Dropped is not composited: it never reached the screen, and a
        // caller treating the two alike would stop repainting a layer nobody
        // can see.
        assert!(!report.was_composited(LayerId(3)));
        assert!(!report.was_composited(LayerId(9)), "absent");
    }
}
