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
    /// The plane armed beside each test's assignment, and whether it counts
    /// against `max_planes` the way a plane-limited controller counts it.
    extra: Option<u32>,
    extras_seen: Vec<Option<u32>>,
    count_extra: bool,
}

impl TestCommitter for Committer {
    fn test_assignment(&mut self, assignment: &[(u32, LayerRef<'_>)]) -> Result<(), TestFailure> {
        self.calls += 1;
        let mut planes: Vec<u32> = assignment.iter().map(|(id, _)| *id).collect();
        planes.sort_unstable();
        self.seen.push(planes);
        self.extras_seen.push(self.extra);
        let armed = assignment.len() + usize::from(self.count_extra && self.extra.is_some());

        if let Some(failure) = self.fail_with {
            return Err(failure);
        }
        if self.calls <= self.reject_first {
            return Err(TestFailure::Rejected);
        }
        if self.max_planes.is_some_and(|max| armed > max) {
            return Err(TestFailure::Rejected);
        }
        Ok(())
    }

    fn set_extra_plane(&mut self, plane_id: u32, _layer: Layer) {
        self.extra = Some(plane_id);
    }

    fn clear_extra_plane(&mut self) {
        self.extra = None;
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

/// Planes with no settable `zpos`, where index order *is* stacking order.
///
/// vkms is like this, and so are plenty of embedded display controllers,
/// `Tegra` Orin's display block among them. `zpos: None` is not "the plane has no
/// preference"; it is "nothing can reorder these".
fn fixed_order_registry() -> PlaneRegistry {
    PlaneRegistry::from_capabilities(vec![
        plane(31, PlaneType::Primary, None),
        plane(32, PlaneType::Overlay, None),
        plane(33, PlaneType::Overlay, None),
        plane(34, PlaneType::Overlay, None),
    ])
}

/// The bottom layer gets the bottom plane, whatever order it arrived in.
///
/// On a card whose planes cannot be reordered, the plane a layer lands on is
/// the only thing that decides what covers what. Assigning in the order layers
/// happen to be held — which is arrival order, and after a removal and two
/// additions is not zpos order — puts a backing store on a higher-numbered
/// plane than the overlays it is meant to sit behind, and it covers them.
///
/// Nothing in the commit report shows this. Every layer is assigned, the
/// counts balance, the kernel accepts it, and the screen is wrong.
#[test]
fn layers_are_assigned_bottom_up_when_planes_cannot_be_reordered() {
    let registry = fixed_order_registry();
    let mut allocator = Allocator::new();
    let mut committer = Committer::default();

    // Deliberately out of zpos order, the way a scene is after a layer is
    // dropped and two are added in its place.
    let top = layer_at(400, 200, 256, 256, 2);
    let bottom = layer_at(0, 0, 1024, 768, 0);
    let middle = layer_at(64, 64, 256, 256, 1);
    let layers = [
        LayerRef {
            id: LayerId(10),
            layer: &top,
        },
        LayerRef {
            id: LayerId(11),
            layer: &bottom,
        },
        LayerRef {
            id: LayerId(12),
            layer: &middle,
        },
    ];

    let allocation = allocator
        .allocate(&layers, &registry, 0, &mut committer)
        .expect("a valid assignment exists");

    let plane_of = |id: LayerId| {
        allocation
            .assignment
            .get_plane_of(id)
            .unwrap_or_else(|| panic!("{id:?} was not placed"))
    };
    let (bottom_plane, middle_plane, top_plane) = (
        plane_of(LayerId(11)),
        plane_of(LayerId(12)),
        plane_of(LayerId(10)),
    );

    assert!(
        bottom_plane < middle_plane && middle_plane < top_plane,
        "stacking is plane order here, so zpos 0/1/2 must land on ascending \
         planes; got {bottom_plane}, {middle_plane}, {top_plane}"
    );
}

// --- candidate_modifiers -----------------------------------------------------

/// A plane advertising exactly `modifiers` for `XRGB8888`.
fn plane_with(id: u32, plane_type: PlaneType, modifiers: &[u64]) -> PlaneCapabilities {
    let mut cap = plane(id, plane_type, None);
    cap.has_format_modifiers = true;
    cap.format_table = drmkit_fmt::FormatTable::from_pairs(
        modifiers
            .iter()
            .map(|m| (fourcc::XRGB8888, drmkit_fmt::Modifier(*m))),
    );
    cap
}

const LINEAR: u64 = 0;
/// `AFBC(16x16)` on ARM, standing in for a compressed primary-only layout.
const AFBC: u64 = 0x0800_0000_0000_0001;

/// The union is taken across every non-cursor plane, not the primary alone.
///
/// This is the split-`SoC` shape the union exists for: the primary scans out
/// only the compressed layout, and a producer that exports `LINEAR` would
/// intersect to empty against it -- missing the overlay that can take the
/// layer, which is where the allocator was going to put it anyway.
#[test]
fn a_linear_only_overlay_is_still_a_candidate_when_the_primary_is_compressed_only() {
    let registry = PlaneRegistry::from_capabilities(vec![
        plane_with(31, PlaneType::Primary, &[AFBC]),
        plane_with(32, PlaneType::Overlay, &[LINEAR]),
    ]);

    assert_eq!(
        registry.candidate_modifiers(0, fourcc::XRGB8888),
        vec![LINEAR, AFBC],
        "intersecting against the primary alone would answer nothing here"
    );
}

/// A modifier two planes share is offered once.
#[test]
fn a_modifier_several_planes_share_is_reported_once() {
    let registry = PlaneRegistry::from_capabilities(vec![
        plane_with(31, PlaneType::Primary, &[LINEAR, AFBC]),
        plane_with(32, PlaneType::Overlay, &[LINEAR]),
        plane_with(33, PlaneType::Overlay, &[LINEAR]),
    ]);

    assert_eq!(
        registry.candidate_modifiers(0, fourcc::XRGB8888),
        vec![LINEAR, AFBC]
    );
}

/// The cursor plane is not a scanout candidate, so what it can scan out is
/// not offered to the producer.
///
/// Offering it would be a real error rather than a cosmetic one: a producer
/// told a cursor-only modifier is available can allocate in it, and no plane
/// the layer may land on will take the result.
#[test]
fn the_cursor_planes_modifiers_are_not_offered() {
    let registry = PlaneRegistry::from_capabilities(vec![
        plane_with(31, PlaneType::Primary, &[LINEAR]),
        plane_with(34, PlaneType::Cursor, &[AFBC]),
    ]);

    assert_eq!(
        registry.candidate_modifiers(0, fourcc::XRGB8888),
        vec![LINEAR],
        "the cursor path owns that plane; a scanout layer never lands there"
    );
}

/// A format no plane advertises has no candidates, which is the answer that
/// sends the caller to the `LINEAR` fallback rather than an empty allocation.
#[test]
fn a_format_no_plane_scans_out_has_no_candidates() {
    let registry =
        PlaneRegistry::from_capabilities(vec![plane_with(31, PlaneType::Primary, &[LINEAR])]);

    assert!(registry.candidate_modifiers(0, fourcc::NV12).is_empty());
}

/// Planes on another CRTC are not candidates.
#[test]
fn a_plane_on_another_crtc_contributes_nothing() {
    let mut other = plane_with(32, PlaneType::Overlay, &[AFBC]);
    other.possible_crtcs = 0b10;
    let registry = PlaneRegistry::from_capabilities(vec![
        plane_with(31, PlaneType::Primary, &[LINEAR]),
        other,
    ]);

    assert_eq!(
        registry.candidate_modifiers(0, fourcc::XRGB8888),
        vec![LINEAR]
    );
}

// --- zpos stacking (drm-cxx `8bf20e6`, `ad47fa9`) -----------------------------

/// A layer above 0 can take a lone fixed-slot primary.
///
/// On a single-plane controller (i.MX LCDIF, tilcdc) the primary is pinned at
/// 0, and requiring the layer's zpos to equal the slot composited every layer
/// above 0, even alone. Nothing else is placed, so there is no order to break.
#[test]
fn a_layer_above_zero_can_take_a_lone_fixed_primary() {
    let registry =
        PlaneRegistry::from_capabilities(vec![plane(31, PlaneType::Primary, Some((0, 0)))]);
    let layers = vec![(LayerId(1), layer_at(0, 0, 640, 480, 1))];
    let mut allocator = Allocator::new();

    let result = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    assert_eq!(result.assignment.get(31), Some(LayerId(1)));
    assert!(result.composited.is_empty());
}

/// A requested zpos past a mutable overlay's range still reaches it; the
/// stacked value is what gets written.
#[test]
fn a_zpos_past_the_overlays_range_still_reaches_it() {
    let layers = vec![(LayerId(1), layer_at(0, 0, 640, 480, 9))];
    let mut allocator = Allocator::new();

    let result = allocator
        .allocate(&refs(&layers), &registry(), 0, &mut Committer::default())
        .expect("allocate");

    assert_eq!(
        result.assignment.len(),
        1,
        "zpos 9 against overlays of [1, 4]"
    );
}

/// The allocator never picks an inverted stack, which the kernel would accept.
///
/// amdgpu's primary is pinned at 2. With a layer asking 3 and one asking 5,
/// putting the 5 on the primary and the 3 on an overlay stacks them upside
/// down -- the overlay is written at 3, above the slot -- which no TEST can
/// tell apart from a correct frame. (Below the slot is fine: an overlay under
/// the primary is an underlay, and a layer asking 1 may take one.)
#[test]
fn an_inverted_stack_is_never_chosen() {
    let registry = PlaneRegistry::from_capabilities(vec![
        plane(40, PlaneType::Primary, Some((2, 2))),
        plane(41, PlaneType::Overlay, Some((0, 255))),
    ]);
    let layers = vec![
        (LayerId(1), layer_at(0, 0, 640, 480, 5)),
        (LayerId(2), layer_at(0, 0, 640, 480, 3)),
    ];
    let mut allocator = Allocator::new();

    let result = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    let inverted = result.assignment.get(40) == Some(LayerId(1))
        && result.assignment.get(41) == Some(LayerId(2));
    assert!(
        !inverted,
        "{:?} stacks the 3 above the 5",
        result.assignment.entries()
    );
}

/// A warm start whose cached assignment no longer stacks is not reused.
///
/// Last frame the layer on the fixed primary asked 5 and the one on the overlay
/// asked 1, an underlay -- consistent. When the overlay's request rises to 3 it
/// is written above the primary's slot while asking to sit below the 5, and
/// re-validating the cached assignment with a TEST would pass, so the stacking
/// check has to run on the warm path too.
#[test]
fn a_warm_start_that_would_invert_the_stack_is_not_reused() {
    let registry = PlaneRegistry::from_capabilities(vec![
        plane(40, PlaneType::Primary, Some((2, 2))),
        plane(41, PlaneType::Overlay, Some((0, 255))),
    ]);
    let mut layers = vec![
        (LayerId(1), layer_at(0, 0, 640, 480, 5)),
        (LayerId(2), layer_at(0, 0, 640, 480, 1)),
    ];
    let mut allocator = Allocator::new();
    let first = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");
    assert_eq!(first.assignment.get(40), Some(LayerId(1)));
    assert_eq!(first.assignment.get(41), Some(LayerId(2)));

    layers[1].1.set_property(PropTag::Zpos, 3);
    let second = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    let inverted = second.assignment.get(40) == Some(LayerId(1))
        && second.assignment.get(41) == Some(LayerId(2));
    assert!(
        !inverted,
        "the cached assignment now inverts the stack and was kept"
    );
}

// --- plane order (drm-cxx 03bc1d7) -------------------------------------------

/// `count` layers stacked on one spot at zpos `0..count`, ids `1..=count`.
fn stacked_layers(count: u64) -> Vec<(LayerId, Layer)> {
    (0..count)
        .map(|zpos| (LayerId(zpos + 1), layer_at(0, 0, 64, 64, zpos)))
        .collect()
}

/// The planes each layer landed on, in zpos order; `None` for composited.
fn planes_in_zpos_order(
    allocation: &drmkit_planes::Allocation,
    layers: &[(LayerId, Layer)],
) -> Vec<Option<u32>> {
    let mut by_zpos: Vec<&(LayerId, Layer)> = layers.iter().collect();
    by_zpos.sort_by_key(|(_, layer)| layer.property(PropTag::Zpos));
    by_zpos
        .iter()
        .map(|(id, _)| allocation.assignment.get_plane_of(*id))
        .collect()
}

/// More layers than planes. What is left over is one contiguous zpos run, and
/// the canvas plane carrying it sits between the run's neighbors: the canvas
/// reservation used to take a plane below the layers it carries (drm-cxx#240).
#[test]
fn overflow_composites_one_run_on_a_canvas_between_its_neighbors() {
    let registry = fixed_order_registry();
    let layers = stacked_layers(6);
    let mut allocator = Allocator::new();
    allocator.set_canvas(&[31, 32, 33, 34], Some(&Layer::new()));

    let allocation = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    assert_eq!(
        planes_in_zpos_order(&allocation, &layers),
        [None, None, None, Some(32), Some(33), Some(34)],
        "three placed on planes, the run below them on the canvas"
    );
    assert_eq!(allocator.canvas_plane(), Some(31));

    // The warm start keeps the canvas plane with the assignment.
    let again = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");
    assert_eq!(again.assignment.entries(), allocation.assignment.entries());
    assert_eq!(allocator.canvas_plane(), Some(31));
}

/// Under plane pressure only the low-priority layers go to the canvas, though
/// they sit mid-stack: the run lands where it costs least, and the canvas
/// sits between the layers above and below it.
#[test]
fn the_composited_run_takes_the_low_priority_layers() {
    let registry = fixed_order_registry();
    let mut layers = stacked_layers(6);
    for (id, layer) in &mut layers {
        layer.set_app_priority(if (3..=5).contains(&id.0) { 10 } else { 200 });
    }
    let mut allocator = Allocator::new();
    allocator.set_canvas(&[31, 32, 33, 34], Some(&Layer::new()));

    let allocation = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    assert_eq!(
        planes_in_zpos_order(&allocation, &layers),
        [Some(31), Some(32), None, None, None, Some(34)]
    );
    assert_eq!(allocator.canvas_plane(), Some(33));
}

/// Reversing the stack after a steady frame must move the layers. The cached
/// assignment is still valid to the kernel, just stacked in the old order, so
/// a warm start that only asks the kernel keeps it (drm-cxx#239).
#[test]
fn a_restack_is_not_served_from_the_warm_start() {
    let registry = fixed_order_registry();
    let mut layers = stacked_layers(3);
    let mut allocator = Allocator::new();
    allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    for (index, (_, layer)) in layers.iter_mut().enumerate() {
        layer.set_property(PropTag::Zpos, 2 - index as u64);
    }
    let restacked = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    let planes = planes_in_zpos_order(&restacked, &layers);
    assert!(
        planes.windows(2).all(|pair| pair[0] < pair[1]),
        "zpos order must be plane order after the restack; got {planes:?}"
    );
}

/// A refused frame retries with one placed layer fewer, and out of tests the
/// whole stack goes to the canvas on the topmost host rather than arming an
/// untested assignment.
#[test]
fn out_of_tests_the_whole_stack_goes_to_the_canvas() {
    let registry = fixed_order_registry();
    let layers = stacked_layers(3);
    let mut allocator = Allocator::new();
    allocator.set_canvas(&[31, 32, 33, 34], Some(&Layer::new()));
    allocator.set_max_test_commits(2);
    let mut committer = Committer {
        reject_first: usize::MAX,
        ..Committer::default()
    };

    let allocation = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("allocate");

    // With nothing composited the stack is fit top-down, onto the top planes.
    assert_eq!(committer.seen, [vec![32, 33, 34], vec![33, 34]]);
    assert!(allocation.assignment.is_empty());
    assert_eq!(allocation.composited.len(), 3);
    assert!(allocation.diagnostics.budget_exhausted);
    assert_eq!(allocator.canvas_plane(), Some(34));
}

/// The cursor plane is the cursor path's, and a pinned layer's plane is the
/// scene's: neither is offered. Without a canvas the layer left over is
/// dropped and no canvas plane is named.
#[test]
fn plane_order_leaves_the_cursor_and_pinned_planes_alone() {
    let registry = PlaneRegistry::from_capabilities(vec![
        plane(31, PlaneType::Primary, None),
        plane(32, PlaneType::Overlay, None),
        plane(33, PlaneType::Overlay, None),
        plane(34, PlaneType::Cursor, None),
    ]);
    let mut layers = stacked_layers(4);
    layers[3].1.set_pinned(true).set_assigned_plane(Some(33));
    let mut allocator = Allocator::new();

    let allocation = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    let used: Vec<u32> = allocation
        .assignment
        .entries()
        .iter()
        .map(|(plane, _)| *plane)
        .collect();
    assert_eq!(used.len(), 2, "two free planes for three layers: {used:?}");
    assert!(!used.contains(&33) && !used.contains(&34), "{used:?}");
    assert_eq!(allocation.composited.len(), 1);
    assert_eq!(allocator.canvas_plane(), None);
}

/// Equal zpos asks for no order, so a tie under pressure composites its
/// lowest-priority layers whatever order the caller added them in. Keeping
/// caller order would make the run take whichever came first.
#[test]
fn a_zpos_tie_composites_its_lowest_priority_layers() {
    let registry = fixed_order_registry();
    let mut layers: Vec<(LayerId, Layer)> = (1..=6)
        .map(|id| (LayerId(id), layer_at(0, 0, 64, 64, 0)))
        .collect();
    for (id, layer) in &mut layers {
        // Highest first, so caller order is the worst order.
        layer.set_app_priority(u8::try_from(250 - id.0 * 10).expect("small"));
    }
    let mut allocator = Allocator::new();
    allocator.set_canvas(&[31, 32, 33, 34], Some(&Layer::new()));

    let allocation = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    let mut composited: Vec<u64> = allocation.composited.iter().map(|id| id.0).collect();
    composited.sort_unstable();
    assert_eq!(composited, [4, 5, 6], "the three lowest priorities");
}

// --- warm start with composited layers (drm-cxx 6e58d23, #341) ----------------

/// Six layers stacked on four id-ordered planes with a canvas: three placed,
/// three composited, the canvas taking the last plane.
fn composited_scene(low: &[u64]) -> Vec<(LayerId, Layer)> {
    let mut layers = stacked_layers(6);
    for (id, layer) in &mut layers {
        layer.set_app_priority(if low.contains(&id.0) { 10 } else { 200 });
    }
    layers
}

fn composited_ids(allocation: &drmkit_planes::Allocation) -> Vec<u64> {
    let mut ids: Vec<u64> = allocation.composited.iter().map(|id| id.0).collect();
    ids.sort_unstable();
    ids
}

/// With every plane taken, a layer composited last frame is not new: the warm
/// start composites it again rather than paying for a full search. Shown by
/// what a full search would do differently -- move the run to the layers that
/// are now cheap.
#[test]
fn a_composited_layer_is_not_new_while_no_plane_is_free() {
    let registry = fixed_order_registry();
    let mut layers = composited_scene(&[1, 2, 3]);
    let mut allocator = Allocator::new();
    allocator.set_canvas(&[31, 32, 33, 34], Some(&Layer::new()));
    let first = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");
    assert_eq!(composited_ids(&first), [1, 2, 3]);

    for (id, layer) in &mut layers {
        layer.set_app_priority(if id.0 >= 4 { 10 } else { 200 });
    }
    let second = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    assert_eq!(second.diagnostics.test_commits_issued, 1);
    assert_eq!(second.assignment.entries(), first.assignment.entries());
    assert_eq!(composited_ids(&second), [1, 2, 3]);
    assert_eq!(allocator.canvas_plane(), Some(31));
}

/// Removing composited layers until the one left would fit its canvas's
/// plane: a full search gives it that plane and retires the canvas, where a
/// warm start would go on blending one layer every frame.
#[test]
fn a_lone_composited_layer_takes_the_canvas_plane_once_it_fits() {
    let registry = fixed_order_registry();
    let mut layers = composited_scene(&[1, 2, 3]);
    let mut allocator = Allocator::new();
    allocator.set_canvas(&[31, 32, 33, 34], Some(&Layer::new()));
    allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    layers.retain(|(id, _)| id.0 != 2 && id.0 != 3);
    let shrunk = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    assert!(shrunk.composited.is_empty(), "{:?}", shrunk.composited);
    assert_eq!(shrunk.assignment.len(), 4);
    assert_eq!(allocator.canvas_plane(), None);
}

/// The shape of drm-cxx#341: placed layers removed so the scene is back
/// under the plane count. Every layer gets a plane and the canvas goes.
#[test]
fn a_scene_back_under_the_plane_count_retires_the_canvas() {
    let registry = fixed_order_registry();
    let mut layers = composited_scene(&[1, 2, 3]);
    let mut allocator = Allocator::new();
    allocator.set_canvas(&[31, 32, 33, 34], Some(&Layer::new()));
    allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    layers.retain(|(id, _)| id.0 <= 4);
    let shrunk = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    assert!(shrunk.composited.is_empty(), "{:?}", shrunk.composited);
    assert_eq!(
        planes_in_zpos_order(&shrunk, &layers),
        [Some(31), Some(32), Some(33), Some(34)]
    );
    assert_eq!(allocator.canvas_plane(), None);
}

/// On a controller that lights fewer planes than it offers, the search
/// composites with planes free -- and a free plane alone must not send every
/// later frame back to search, or the settled scene pays the whole descent
/// each frame to reach the same answer (drm-cxx `893a938`'s steady state).
#[test]
fn a_plane_limit_verdict_holds_while_its_planes_stay_free() {
    let registry = fixed_order_registry();
    let layers = stacked_layers(3);
    let mut allocator = Allocator::new();
    allocator.set_canvas(&[31, 32, 33, 34], Some(&Layer::new()));
    // One plane besides the canvas, which this committer does not count.
    let mut committer = Committer {
        max_planes: Some(1),
        ..Committer::default()
    };

    let first = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("allocate");
    assert_eq!(first.assignment.len(), 1);
    assert_eq!(first.composited.len(), 2);

    let second = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("allocate");
    assert_eq!(
        second.diagnostics.test_commits_issued, 1,
        "one warm-start test"
    );
    assert_eq!(second.assignment.entries(), first.assignment.entries());
}

/// A primary and three overlays, every one taking a settable `zpos`.
fn zpos_registry() -> PlaneRegistry {
    PlaneRegistry::from_capabilities(vec![
        plane(31, PlaneType::Primary, Some((0, 0))),
        plane(32, PlaneType::Overlay, Some((1, 8))),
        plane(33, PlaneType::Overlay, Some((1, 8))),
        plane(34, PlaneType::Overlay, Some((1, 8))),
    ])
}

/// Where planes take a `zpos`, a plane held for the canvas is armed in every
/// test, so a controller that lights fewer planes than it offers is asked
/// about the frame it will get. The tests used to leave it out and the frame
/// committed one plane over the limit (the SA8155P: three armed on two).
/// The warm start then re-tests the pair, once, rather than searching again.
#[test]
fn a_held_canvas_plane_is_tested_with_the_layers_and_kept_with_them() {
    let registry = zpos_registry();
    let layers = stacked_layers(3);
    let mut allocator = Allocator::new();
    allocator.set_canvas(&[32, 33, 34], Some(&Layer::new()));
    let mut committer = Committer {
        max_planes: Some(2),
        count_extra: true,
        ..Committer::default()
    };

    allocator.set_reserved_planes(&[34]);
    allocator.hold_canvas_plane(Some(34));
    let first = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("allocate");
    assert_eq!(first.assignment.len(), 1, "{:?}", first.assignment);
    assert_eq!(first.composited.len(), 2);
    assert_eq!(allocator.canvas_plane(), Some(34));
    assert!(
        committer.extras_seen.iter().all(|extra| *extra == Some(34)),
        "every test arms the canvas: {:?}",
        committer.extras_seen
    );

    // The next frame holds nothing up front, as the scene's first pass does.
    allocator.set_reserved_planes(&[]);
    allocator.hold_canvas_plane(None);
    committer.extras_seen.clear();
    let second = allocator
        .allocate(&refs(&layers), &registry, 0, &mut committer)
        .expect("allocate");
    assert_eq!(
        second.diagnostics.test_commits_issued, 1,
        "one warm-start test"
    );
    assert_eq!(committer.extras_seen, [Some(34)], "with the canvas armed");
    assert_eq!(second.assignment.entries(), first.assignment.entries());
    assert_eq!(allocator.canvas_plane(), Some(34));
}

/// Once the layers fit, the plane the canvas held goes back to them (drm-cxx
/// `8e73f82`): a reused scene that shrank is not held in composition by the
/// canvas's own plane.
#[test]
fn a_held_canvas_plane_goes_back_to_the_layers_once_they_fit() {
    let registry = zpos_registry();
    let mut layers = stacked_layers(5);
    let mut allocator = Allocator::new();
    allocator.set_canvas(&[32, 33, 34], Some(&Layer::new()));

    allocator.set_reserved_planes(&[34]);
    allocator.hold_canvas_plane(Some(34));
    let first = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");
    assert_eq!(first.composited.len(), 2, "{:?}", first.composited);

    let gone = first.composited[0];
    layers.retain(|(id, _)| *id != gone);
    allocator.set_reserved_planes(&[]);
    allocator.hold_canvas_plane(None);
    let shrunk = allocator
        .allocate(&refs(&layers), &registry, 0, &mut Committer::default())
        .expect("allocate");

    assert!(shrunk.composited.is_empty(), "{:?}", shrunk.composited);
    assert_eq!(shrunk.assignment.len(), 4);
    assert_eq!(allocator.canvas_plane(), None);
}
