// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of `tests/unit/test_plane_allocator.cpp` from drm-cxx @
//! `dc2915b`, against synthetic `PlaneRegistry::from_capabilities` fixtures —
//! no hardware, which is the phase 2 gate's requirement.

use drmkit_fmt::fourcc;
use drmkit_planes::{
    Allocator, ContentType, Layer, LayerId, LayerRef, PlaneCapabilities, PlaneRegistry, PlaneType,
    PropTag, TestCommitter, TestFailure,
};

// --- fixtures ----------------------------------------------------------------

fn plane(id: u32, plane_type: PlaneType, zpos: Option<(u64, u64)>) -> PlaneCapabilities {
    PlaneCapabilities {
        id,
        possible_crtcs: 0b1,
        plane_type,
        formats: vec![fourcc::XRGB8888, fourcc::ARGB8888],
        zpos_min: zpos.map(|(min, _)| min),
        zpos_max: zpos.map(|(_, max)| max),
        supports_scaling: true,
        ..PlaneCapabilities::default()
    }
}

/// One primary plus two overlays, the shape most cases use.
fn registry() -> PlaneRegistry {
    PlaneRegistry::from_capabilities(vec![
        plane(31, PlaneType::Primary, Some((0, 0))),
        plane(32, PlaneType::Overlay, Some((1, 4))),
        plane(33, PlaneType::Overlay, Some((1, 4))),
    ])
}

fn layer_at(x: i32, y: i32, w: u32, h: u32, zpos: u64) -> Layer {
    let mut layer = Layer::new();
    layer
        .set_property(PropTag::PixelFormat, u64::from(fourcc::XRGB8888))
        .set_property(PropTag::FbId, 1)
        .set_property(PropTag::CrtcX, u64::from(x.cast_unsigned()))
        .set_property(PropTag::CrtcY, u64::from(y.cast_unsigned()))
        .set_property(PropTag::CrtcW, u64::from(w))
        .set_property(PropTag::CrtcH, u64::from(h))
        .set_property(PropTag::SrcW, u64::from(w) << 16)
        .set_property(PropTag::SrcH, u64::from(h) << 16)
        .set_property(PropTag::Zpos, zpos);
    layer
}

/// Counts test commits and can be told to reject the first `reject_first` of
/// them, so a case can drive the search down to greedy or backtrack.
#[derive(Debug, Default)]
struct Committer {
    calls: usize,
    reject_first: usize,
    /// Reject any assignment larger than this, modeling scarce bandwidth.
    max_planes: Option<usize>,
    fail_with: Option<TestFailure>,
    seen: Vec<Vec<u32>>,
}

impl TestCommitter for Committer {
    fn test_assignment(&mut self, assignment: &[(u32, LayerRef<'_>)]) -> Result<(), TestFailure> {
        self.calls += 1;
        let mut planes: Vec<u32> = assignment.iter().map(|(id, _)| *id).collect();
        planes.sort_unstable();
        self.seen.push(planes);

        if let Some(failure) = self.fail_with {
            return Err(failure);
        }
        if self.calls <= self.reject_first {
            return Err(TestFailure::Rejected);
        }
        if self.max_planes.is_some_and(|max| assignment.len() > max) {
            return Err(TestFailure::Rejected);
        }
        Ok(())
    }
}

fn refs(layers: &[(LayerId, Layer)]) -> Vec<LayerRef<'_>> {
    layers
        .iter()
        .map(|(id, layer)| LayerRef { id: *id, layer })
        .collect()
}

// --- placement ---------------------------------------------------------------

#[test]
fn places_every_layer_when_planes_are_plentiful() {
    let layers = vec![
        (LayerId(1), layer_at(0, 0, 1920, 1080, 0)),
        (LayerId(2), layer_at(0, 0, 640, 480, 1)),
    ];
    let mut allocator = Allocator::new();
    let mut committer = Committer::default();

    let result = allocator
        .allocate(&refs(&layers), &registry(), 0, &mut committer)
        .expect("allocate");

    assert_eq!(result.assignment.len(), 2);
    assert!(result.composited.is_empty());
    assert_eq!(
        result.diagnostics.test_commits_issued, 1,
        "preseed passes first try"
    );
    assert!(!result.diagnostics.fb_delta_fast_path);
}

#[test]
fn a_layer_with_no_format_is_never_placed() {
    let mut bare = Layer::new();
    bare.set_property(PropTag::CrtcW, 100)
        .set_property(PropTag::CrtcH, 100);
    let layers = vec![(LayerId(1), bare)];

    let mut allocator = Allocator::new();
    let mut committer = Committer::default();
    let result = allocator
        .allocate(&refs(&layers), &registry(), 0, &mut committer)
        .expect("allocate");

    assert!(result.assignment.is_empty());
    assert_eq!(result.composited, vec![LayerId(1)]);
}

#[test]
fn force_composited_layers_are_routed_to_composition() {
    let mut composited = layer_at(0, 0, 640, 480, 1);
    composited.set_force_composited(true);
    let layers = vec![
        (LayerId(1), layer_at(0, 0, 1920, 1080, 0)),
        (LayerId(2), composited),
    ];

    let mut allocator = Allocator::new();
    let mut committer = Committer::default();
    let result = allocator
        .allocate(&refs(&layers), &registry(), 0, &mut committer)
        .expect("allocate");

    assert_eq!(result.assignment.len(), 1);
    assert_eq!(result.composited, vec![LayerId(2)]);
}

/// Externally-bound and pinned layers are the scene's business: the allocator
/// must not place them and must not report them for composition either.
#[test]
fn externally_bound_and_pinned_layers_are_skipped_entirely() {
    let mut bound = layer_at(0, 0, 640, 480, 1);
    bound.set_externally_bound(true);
    let mut pinned = layer_at(0, 0, 320, 240, 2);
    pinned.set_pinned(true);

    let layers = vec![
        (LayerId(1), layer_at(0, 0, 1920, 1080, 0)),
        (LayerId(2), bound),
        (LayerId(3), pinned),
    ];

    let mut allocator = Allocator::new();
    let mut committer = Committer::default();
    let result = allocator
        .allocate(&refs(&layers), &registry(), 0, &mut committer)
        .expect("allocate");

    assert_eq!(result.assignment.len(), 1);
    assert!(
        result.composited.is_empty(),
        "the scene owns these layers; the allocator neither places nor composites them"
    );
}

/// Under plane pressure the higher content class survives.
#[test]
fn backtracking_drops_the_lowest_priority_layer() {
    let mut video = layer_at(0, 0, 1920, 1080, 1);
    video.set_content_type(ContentType::Video);
    let ui = layer_at(0, 0, 1920, 1080, 2);

    let layers = vec![(LayerId(1), video), (LayerId(2), ui)];
    let mut allocator = Allocator::new();
    // Only one plane may be active at a time.
    let mut committer = Committer {
        max_planes: Some(1),
        ..Committer::default()
    };

    let result = allocator
        .allocate(&refs(&layers), &registry(), 0, &mut committer)
        .expect("allocate");

    assert_eq!(result.assignment.len(), 1);
    assert_eq!(
        result.assignment.entries()[0].1,
        LayerId(1),
        "the Video layer must survive; the Generic one is dropped"
    );
    assert_eq!(result.composited, vec![LayerId(2)]);
}

#[test]
fn lost_master_propagates_rather_than_retrying() {
    let layers = vec![(LayerId(1), layer_at(0, 0, 1920, 1080, 0))];
    let mut allocator = Allocator::new();
    let mut committer = Committer {
        fail_with: Some(TestFailure::NotMaster),
        ..Committer::default()
    };

    assert!(matches!(
        allocator.allocate(&refs(&layers), &registry(), 0, &mut committer),
        Err(TestFailure::NotMaster)
    ));
    assert_eq!(
        committer.calls, 1,
        "no smaller assignment can help, so do not keep asking"
    );
}

#[test]
fn the_test_commit_budget_is_respected() {
    let layers: Vec<(LayerId, Layer)> = (0..3)
        .map(|i| (LayerId(i + 1), layer_at(0, 0, 1920, 1080, i)))
        .collect();

    let mut allocator = Allocator::new();
    allocator.set_max_test_commits(2);
    // Reject everything, so the search would otherwise keep backtracking.
    let mut committer = Committer {
        reject_first: usize::MAX,
        ..Committer::default()
    };

    let result = allocator
        .allocate(&refs(&layers), &registry(), 0, &mut committer)
        .expect("allocate");

    assert!(
        committer.calls <= 2,
        "made {} test commits",
        committer.calls
    );
    assert_eq!(result.diagnostics.test_commits_issued, committer.calls);
}

// --- invariant 4: the FB-only fast path --------------------------------------

/// **Invariant 4.** A frame that changes only `FB_ID` on already-placed layers
/// must reuse the cached assignment and issue **zero** test commits.
#[test]
fn fb_only_frame_skips_the_test_commit() {
    let mut layers = vec![
        (LayerId(1), layer_at(0, 0, 1920, 1080, 0)),
        (LayerId(2), layer_at(0, 0, 640, 480, 1)),
    ];
    let mut allocator = Allocator::new();
    let mut committer = Committer::default();
    let registry = registry();

    // Frame 1: full search.
    let first = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("frame 1");
    assert_eq!(first.assignment.len(), 2);
    assert!(first.diagnostics.test_commits_issued > 0);

    // The scene records what the kernel accepted.
    for (plane_id, layer_id) in first.assignment.entries() {
        let entry = refs(&layers)
            .into_iter()
            .find(|e| e.id == *layer_id)
            .expect("layer");
        allocator.record_committed(*plane_id, entry);
    }

    // Frame 2: only the framebuffer changed.
    for (_, layer) in &mut layers {
        layer.set_property(PropTag::FbId, 99);
    }

    let calls_before = committer.calls;
    let second = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("frame 2");

    assert!(
        second.diagnostics.fb_delta_fast_path,
        "the fast path must engage"
    );
    assert_eq!(second.diagnostics.test_commits_issued, 0);
    assert_eq!(committer.calls, calls_before, "no test commit was issued");
    assert_eq!(second.assignment, first.assignment, "same placement reused");
}

/// The other half: a *placement* change must defeat the fast path, because the
/// cached assignment is no longer known good.
#[test]
fn a_placement_change_defeats_the_fast_path() {
    let mut layers = vec![(LayerId(1), layer_at(0, 0, 1920, 1080, 0))];
    let mut allocator = Allocator::new();
    let mut committer = Committer::default();
    let registry = registry();

    let first = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("frame 1");
    for (plane_id, layer_id) in first.assignment.entries() {
        let entry = refs(&layers)
            .into_iter()
            .find(|e| e.id == *layer_id)
            .unwrap();
        allocator.record_committed(*plane_id, entry);
    }

    // Move the layer: geometry is placement, so the hash moves.
    layers[0].1.set_property(PropTag::CrtcX, 64);

    let second = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("frame 2");

    assert!(
        !second.diagnostics.fb_delta_fast_path,
        "a geometry change must force re-validation"
    );
    assert!(second.diagnostics.test_commits_issued > 0);
}

/// A previously-placed layer disappearing also defeats it.
#[test]
fn a_vanished_layer_defeats_the_fast_path() {
    let layers = vec![
        (LayerId(1), layer_at(0, 0, 1920, 1080, 0)),
        (LayerId(2), layer_at(0, 0, 640, 480, 1)),
    ];
    let mut allocator = Allocator::new();
    let mut committer = Committer::default();
    let registry = registry();

    let first = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("frame 1");
    for (plane_id, layer_id) in first.assignment.entries() {
        let entry = refs(&layers)
            .into_iter()
            .find(|e| e.id == *layer_id)
            .unwrap();
        allocator.record_committed(*plane_id, entry);
    }

    // Frame 2 without layer 2.
    let fewer = vec![(LayerId(1), layer_at(0, 0, 1920, 1080, 0))];
    let second = allocator
        .allocate(&refs(&fewer), &registry, 0, &mut committer)
        .expect("frame 2");

    assert!(!second.diagnostics.fb_delta_fast_path);
}

// --- the warm-start stability scenario (plan §4.6) ---------------------------

/// The regression the v2.0.1 stability bonus fixes, and a required T7 corpus
/// member: a compositor inserts a backing-store layer while the overlays stay
/// put. **Every already-placed layer must keep its plane** — reprogramming them
/// all in one commit is what is seen as flicker.
#[test]
fn inserting_a_layer_does_not_reshuffle_the_existing_ones() {
    let overlay_a = layer_at(0, 0, 640, 480, 1);
    let overlay_b = layer_at(700, 0, 640, 480, 2);

    let mut allocator = Allocator::new();
    let mut committer = Committer::default();
    let registry = registry();

    // Frame 1: two overlays.
    let first_layers = vec![
        (LayerId(1), overlay_a.clone()),
        (LayerId(2), overlay_b.clone()),
    ];
    let first = allocator
        .allocate(&refs(&first_layers), &registry, 0, &mut committer)
        .expect("frame 1");
    assert_eq!(first.assignment.len(), 2);

    for (plane_id, layer_id) in first.assignment.entries() {
        let entry = refs(&first_layers)
            .into_iter()
            .find(|e| e.id == *layer_id)
            .unwrap();
        allocator.record_committed(*plane_id, entry);
    }
    let plane_of_1 = first
        .assignment
        .entries()
        .iter()
        .find(|(_, id)| *id == LayerId(1))
        .map(|(p, _)| *p)
        .expect("layer 1 placed");
    let plane_of_2 = first
        .assignment
        .entries()
        .iter()
        .find(|(_, id)| *id == LayerId(2))
        .map(|(p, _)| *p)
        .expect("layer 2 placed");

    // Frame 2: a full-screen backing store appears beneath them. The layer
    // *set* changed, so warm-start does not apply and the search re-solves --
    // the stability bonus is what keeps the existing two where they were.
    let second_layers = vec![
        (LayerId(3), layer_at(0, 0, 1920, 1080, 0)),
        (LayerId(1), overlay_a),
        (LayerId(2), overlay_b),
    ];
    let second = allocator
        .allocate(&refs(&second_layers), &registry, 0, &mut committer)
        .expect("frame 2");

    assert_eq!(
        second.assignment.get(plane_of_1),
        Some(LayerId(1)),
        "layer 1 must keep plane {plane_of_1}: reprogramming it is the flicker"
    );
    assert_eq!(
        second.assignment.get(plane_of_2),
        Some(LayerId(2)),
        "layer 2 must keep plane {plane_of_2}"
    );
}

/// The bonus must never cost a placement — cardinality still wins.
#[test]
fn the_stability_bonus_never_reduces_placements() {
    let mut allocator = Allocator::new();
    let mut committer = Committer::default();
    let registry = registry();

    let first_layers = vec![(LayerId(1), layer_at(0, 0, 640, 480, 1))];
    let first = allocator
        .allocate(&refs(&first_layers), &registry, 0, &mut committer)
        .expect("frame 1");
    for (plane_id, layer_id) in first.assignment.entries() {
        let entry = refs(&first_layers)
            .into_iter()
            .find(|e| e.id == *layer_id)
            .unwrap();
        allocator.record_committed(*plane_id, entry);
    }

    // Frame 2 adds two more layers. All three must be placed.
    let second_layers = vec![
        (LayerId(1), layer_at(0, 0, 640, 480, 1)),
        (LayerId(2), layer_at(700, 0, 640, 480, 2)),
        (LayerId(3), layer_at(0, 600, 640, 400, 0)),
    ];
    let second = allocator
        .allocate(&refs(&second_layers), &registry, 0, &mut committer)
        .expect("frame 2");

    assert_eq!(
        second.assignment.len(),
        3,
        "the stability bonus orders preferences; it must not cost a placement"
    );
}

/// `invalidate_allocation` drops the warm start, so a changed hint can move a
/// layer to the plane it now prefers.
#[test]
fn invalidate_allocation_forces_a_full_search() {
    let layers = vec![(LayerId(1), layer_at(0, 0, 1920, 1080, 0))];
    let mut allocator = Allocator::new();
    let mut committer = Committer::default();
    let registry = registry();

    let first = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("frame 1");
    for (plane_id, layer_id) in first.assignment.entries() {
        let entry = refs(&layers)
            .into_iter()
            .find(|e| e.id == *layer_id)
            .unwrap();
        allocator.record_committed(*plane_id, entry);
    }

    allocator.invalidate_allocation();
    let second = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("frame 2");

    assert!(
        !second.diagnostics.fb_delta_fast_path,
        "invalidation must defeat the fast path too"
    );
    assert!(second.diagnostics.test_commits_issued > 0);
}

/// A new layer arriving in steady state must get a real shot at a plane.
///
/// Both cached paths iterate the *previous* assignment to decide what to emit,
/// so neither can place a layer that is not already in it. Without a check for
/// this, warm-start succeeds with the old set, the new layer is composited, and
/// the cached assignment never grows — so the same fate hits every subsequent
/// frame. The C++ calls this out as a trap; this pins the escape.
#[test]
fn a_new_layer_in_steady_state_is_not_composited_forever() {
    let mut allocator = Allocator::new();
    let mut committer = Committer::default();
    let registry = registry();

    let base = vec![(LayerId(1), layer_at(0, 0, 640, 480, 1))];
    let first = allocator
        .allocate(&refs(&base), &registry, 0, &mut committer)
        .expect("frame 1");
    for (plane_id, layer_id) in first.assignment.entries() {
        let entry = refs(&base).into_iter().find(|e| e.id == *layer_id).unwrap();
        allocator.record_committed(*plane_id, entry);
    }
    assert_eq!(first.assignment.len(), 1);

    // Frame 2: a second layer appears. Everything else is identical, so the
    // FB-only fast path would otherwise engage and strand it.
    let grown = vec![
        (LayerId(1), layer_at(0, 0, 640, 480, 1)),
        (LayerId(2), layer_at(700, 0, 640, 480, 2)),
    ];
    let second = allocator
        .allocate(&refs(&grown), &registry, 0, &mut committer)
        .expect("frame 2");

    assert!(
        !second.diagnostics.fb_delta_fast_path,
        "a new layer must defeat the fast path, or it can never be placed"
    );
    assert_eq!(second.assignment.len(), 2, "the new layer must get a plane");
    assert!(second.composited.is_empty());

    // And the cached assignment grew, so frame 3 is stable rather than
    // repeating the same fate.
    for (plane_id, layer_id) in second.assignment.entries() {
        let entry = refs(&grown)
            .into_iter()
            .find(|e| e.id == *layer_id)
            .unwrap();
        allocator.record_committed(*plane_id, entry);
    }
    let third = allocator
        .allocate(&refs(&grown), &registry, 0, &mut committer)
        .expect("frame 3");
    assert!(
        third.diagnostics.fb_delta_fast_path,
        "with the set stable again, the fast path resumes"
    );
    assert_eq!(third.assignment.len(), 2);
}
