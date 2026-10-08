// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of the `test_layer.cpp` property and hashing cases from
//! drm-cxx @ `dc2915b`, plus the registry fixtures the allocator suite is
//! built on.

use super::*;
use crate::scoring::independent_groups;
use drmkit_fmt::{
    ARM_16X16_BLOCK_U_INTERLEAVED, BandwidthClass, FormatTable, Modifier, fourcc, mod_code, vendor,
};

/// A layer with a plausible full property set, so a test can perturb one
/// property at a time.
fn placed_layer() -> Layer {
    let mut layer = Layer::new();
    layer
        .set_property(PropTag::FbId, 42)
        .set_property(PropTag::FbModifier, 0)
        .set_property(PropTag::CrtcId, 1)
        .set_property(PropTag::CrtcX, 0)
        .set_property(PropTag::CrtcY, 0)
        .set_property(PropTag::CrtcW, 1920)
        .set_property(PropTag::CrtcH, 1080)
        .set_property(PropTag::SrcX, 0)
        .set_property(PropTag::SrcY, 0)
        .set_property(PropTag::SrcW, 1920 << 16)
        .set_property(PropTag::SrcH, 1080 << 16)
        .set_property(PropTag::PixelFormat, u64::from(fourcc::XRGB8888));
    layer
}

// --- invariant 4: the content / placement split -----------------------------

/// **Invariant 4 corollary.** Every property tag must be classified. The
/// exhaustive `match` in `PropTag::class` makes an unclassified variant a
/// compile error; this asserts the other half — that `ALL` really does list
/// every variant, so nothing escapes classification by being left out of the
/// array.
#[test]
fn every_prop_tag_is_classified() {
    assert_eq!(
        PropTag::ALL.len(),
        NUM_PROPS,
        "PropTag::ALL must list every variant"
    );

    let mut seen_indices: Vec<usize> = PropTag::ALL.iter().map(|t| t.index()).collect();
    seen_indices.sort_unstable();
    seen_indices.dedup();
    assert_eq!(
        seen_indices.len(),
        NUM_PROPS,
        "every tag must have a distinct index"
    );
    assert_eq!(
        seen_indices,
        (0..NUM_PROPS).collect::<Vec<_>>(),
        "indices must be contiguous from zero -- the property array is indexed by them"
    );

    // Classification is total by construction; this documents the split.
    for tag in PropTag::ALL {
        let _: PropClass = tag.class();
    }
}

/// The exact membership of the content class, hardcoded.
///
/// **This is the guard for invariant 4's corollary, not the hash test below.**
/// That one derives its expectations from `tag.class()`, so it is self-
/// consistent by construction and passes happily against a misclassification —
/// verified by misclassifying `CRTC_W` and watching it stay green while this
/// case failed. The two divide the work: this pins *what* the classification
/// is, the hash test pins that `property_hash` *uses* it.
///
/// Widening the content set is the dangerous direction: a placement property
/// called content would let a real geometry change skip validation.
#[test]
fn only_fb_id_and_in_fence_fd_are_content() {
    let content: Vec<PropTag> = PropTag::ALL
        .into_iter()
        .filter(|t| t.class() == PropClass::Content)
        .collect();
    assert_eq!(
        content,
        vec![PropTag::FbId, PropTag::InFenceFd],
        "content is exactly FB_ID and IN_FENCE_FD; everything else is placement"
    );
}

/// `LayerTest.PropertyHashIsolatesFbOnlyContentFromPlacement`
///
/// Content mutations leave the hash alone; every placement mutation moves it.
/// Expectations are derived from `tag.class()`, so this proves the hash honors
/// the classification — it cannot prove the classification is right. That is
/// `only_fb_id_and_in_fence_fd_are_content`'s job.
#[test]
fn property_hash_isolates_content_from_placement() {
    let base = placed_layer();
    let base_hash = base.property_hash();

    // Content: the hash must not move.
    for tag in PropTag::ALL
        .into_iter()
        .filter(|t| t.class() == PropClass::Content)
    {
        let mut layer = base.clone();
        layer.set_property(tag, 0xdead_beef);
        assert_eq!(
            layer.property_hash(),
            base_hash,
            "{} is content and must not move the hash",
            tag.name()
        );
    }

    // Placement: every one must move it.
    for tag in PropTag::ALL
        .into_iter()
        .filter(|t| t.class() == PropClass::Placement)
    {
        let mut layer = base.clone();
        let previous = layer.property(tag).unwrap_or(0);
        layer.set_property(tag, previous.wrapping_add(0x5a5a));
        assert_ne!(
            layer.property_hash(),
            base_hash,
            "{} is placement and must move the hash",
            tag.name()
        );
    }
}

/// Setting a placement property that was previously unset must also move the
/// hash — the "newly appeared" case, distinct from "changed value".
#[test]
fn newly_set_placement_property_moves_the_hash() {
    let mut layer = placed_layer();
    let before = layer.property_hash();
    assert_eq!(layer.property(PropTag::Rotation), None);

    layer.set_property(PropTag::Rotation, 1);
    assert_ne!(layer.property_hash(), before);
}

/// The hash is order-independent: two layers with the same logical property
/// set hash the same however they were built.
#[test]
fn property_hash_is_order_independent() {
    let mut forward = Layer::new();
    for tag in PropTag::ALL {
        forward.set_property(tag, tag.index() as u64 * 7 + 1);
    }

    let mut backward = Layer::new();
    for tag in PropTag::ALL.into_iter().rev() {
        backward.set_property(tag, tag.index() as u64 * 7 + 1);
    }

    assert_eq!(forward.property_hash(), backward.property_hash());
}

/// The memoized hash must be invalidated by mutation, or a placement change
/// would be invisible to the fast path.
#[test]
fn property_hash_cache_is_invalidated_on_mutation() {
    let mut layer = placed_layer();
    let first = layer.property_hash();
    assert_eq!(layer.property_hash(), first, "second read is memoized");

    layer.set_property(PropTag::CrtcX, 64);
    assert_ne!(layer.property_hash(), first, "mutation must invalidate");

    layer.disable();
    assert_ne!(layer.property_hash(), first, "disable must invalidate too");
}

/// `LayerTest.DisableSetsFbIdToZero`
///
/// Disabling detaches the framebuffer and nothing else: geometry, format, and
/// stacking survive so a re-enabled layer keeps its placement.
#[test]
fn disable_sets_fb_id_to_zero_and_leaves_the_rest() {
    let mut layer = placed_layer();
    let before = layer.property_hash();

    layer.disable();

    assert_eq!(layer.property(PropTag::FbId), Some(0));
    assert_eq!(
        layer.property(PropTag::CrtcW),
        Some(1920),
        "geometry survives"
    );
    assert_eq!(
        layer.property(PropTag::PixelFormat),
        Some(u64::from(fourcc::XRGB8888)),
        "format survives"
    );
    assert_eq!(
        layer.property_hash(),
        before,
        "FB_ID is content, so disabling must not move the placement hash"
    );
}

/// `LayerTest.SetSameValueDoesNotDirty`
///
/// Rewriting a property's existing value must not dirty the layer. The
/// steady-state path rewrites the same geometry every frame, so treating that
/// as a change would also invalidate the memoized hash each time and defeat the
/// memoization the FB-only fast path leans on.
#[test]
fn setting_the_same_value_is_a_no_op() {
    let mut layer = Layer::new();
    layer.set_property(PropTag::FbId, 42);
    layer.mark_clean();
    assert!(!layer.is_dirty());

    layer.set_property(PropTag::FbId, 42);
    assert!(
        !layer.is_dirty(),
        "an unchanged value must not dirty the layer"
    );

    layer.set_property(PropTag::FbId, 43);
    assert!(layer.is_dirty(), "a real change must");
}

/// The memoized hash must survive a no-op placement write, and still be
/// invalidated by a real one.
#[test]
fn a_no_op_write_does_not_invalidate_the_hash_cache() {
    let mut layer = placed_layer();
    let before = layer.property_hash();

    layer.set_property(PropTag::CrtcW, 1920); // same value
    assert_eq!(layer.property_hash(), before);

    layer.set_property(PropTag::CrtcW, 1280); // different
    assert_ne!(layer.property_hash(), before);
}

// --- property bag ------------------------------------------------------------

#[test]
fn property_names_round_trip() {
    for tag in PropTag::ALL {
        assert_eq!(
            PropTag::from_name(tag.name()),
            Some(tag),
            "{} must round-trip",
            tag.name()
        );
    }
    assert_eq!(PropTag::from_name("NOT_A_PROPERTY"), None);
    assert_eq!(PropTag::from_name(""), None);
}

#[test]
fn set_property_by_name_reports_unknown_names() {
    let mut layer = Layer::new();
    assert!(layer.set_property_by_name("CRTC_W", 640));
    assert_eq!(layer.property(PropTag::CrtcW), Some(640));

    assert!(
        !layer.set_property_by_name("NOT_A_PROPERTY", 1),
        "an unknown name is reported rather than silently dropped"
    );
    assert_eq!(layer.property_count(), 1);
}

#[test]
fn properties_iterate_in_tag_order_and_only_when_set() {
    let mut layer = Layer::new();
    layer
        .set_property(PropTag::Zpos, 3)
        .set_property(PropTag::CrtcId, 1)
        .set_property(PropTag::FbId, 7);

    let seen: Vec<PropTag> = layer.properties().map(|(tag, _)| tag).collect();
    assert_eq!(seen, vec![PropTag::FbId, PropTag::CrtcId, PropTag::Zpos]);
    assert_eq!(layer.property_count(), 3);
}

#[test]
fn snapshot_captures_the_property_state() {
    let layer = placed_layer();
    let snapshot = layer.snapshot();

    assert_eq!(snapshot.get(PropTag::CrtcW), Some(1920));
    assert_eq!(snapshot.get(PropTag::Rotation), None);

    // A snapshot is a value: later mutation does not disturb it.
    let mut layer = layer;
    layer.set_property(PropTag::CrtcW, 640);
    assert_eq!(snapshot.get(PropTag::CrtcW), Some(1920));
    assert_eq!(layer.property(PropTag::CrtcW), Some(640));
}

// --- derived geometry --------------------------------------------------------

#[test]
fn requires_scaling_compares_fixed_point_source_to_whole_pixel_destination() {
    let mut layer = Layer::new();
    layer
        .set_property(PropTag::SrcW, 1920 << 16)
        .set_property(PropTag::SrcH, 1080 << 16)
        .set_property(PropTag::CrtcW, 1920)
        .set_property(PropTag::CrtcH, 1080);
    assert!(!layer.requires_scaling(), "1:1 is not scaling");

    layer.set_property(PropTag::CrtcW, 1280);
    assert!(layer.requires_scaling());

    // A layer missing any of the four reads as unscaled, matching the C++.
    let mut partial = Layer::new();
    partial.set_property(PropTag::SrcW, 1920 << 16);
    assert!(!partial.requires_scaling());
}

#[test]
fn geometry_accessors_default_sensibly() {
    let empty = Layer::new();
    assert_eq!(empty.width(), 0);
    assert_eq!(empty.height(), 0);
    assert_eq!(empty.modifier(), 0, "unset modifier reads as LINEAR");
    assert_eq!(empty.rotation(), 0);
    assert_eq!(empty.format(), None);
    assert_eq!(empty.crtc_rect(), Rect::default());

    let layer = placed_layer();
    assert_eq!(
        layer.crtc_rect(),
        Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080
        }
    );
    assert_eq!(layer.format(), Some(fourcc::XRGB8888));
}

// --- placement flags ---------------------------------------------------------

/// The three ways a layer can be excluded from placement are independent, and
/// `needs_composition` is the union of the two composition flags.
#[test]
fn composition_flags_are_independent() {
    let mut layer = Layer::new();
    assert!(!layer.needs_composition());

    layer.set_force_composited(true);
    assert!(layer.needs_composition());
    assert!(!layer.is_transient_composited());

    layer.set_force_composited(false);
    layer.set_transient_composited(true);
    assert!(
        layer.needs_composition(),
        "the scene's per-commit flag also forces composition"
    );

    layer.set_transient_composited(false);
    assert!(!layer.needs_composition());
}

/// `is_pinned` and `is_externally_bound` both make the allocator skip a layer,
/// but only the externally-bound one gives up its `FB_ID` — a pinned layer is a
/// normal source whose `FB_ID` the scene still writes.
#[test]
fn pinned_and_externally_bound_are_distinct() {
    let mut layer = placed_layer();
    assert!(!layer.is_pinned());
    assert!(!layer.is_externally_bound());

    layer.set_pinned(true);
    assert!(layer.is_pinned());
    assert!(!layer.is_externally_bound());
    assert_eq!(
        layer.property(PropTag::FbId),
        Some(42),
        "a pinned layer keeps its FB_ID"
    );

    layer.set_externally_bound(true);
    assert!(layer.is_pinned() && layer.is_externally_bound());
}

#[test]
fn dirty_tracking_follows_mutation() {
    let mut layer = Layer::new();
    assert!(layer.is_dirty(), "a fresh layer is dirty");

    layer.mark_clean();
    assert!(!layer.is_dirty());

    layer.set_property(PropTag::CrtcX, 1);
    assert!(layer.is_dirty());
}

// --- registry ----------------------------------------------------------------

/// A synthetic overlay plane, the shape the allocator fixtures use.
fn plane(id: u32, plane_type: PlaneType, crtcs: u32, formats: &[u32]) -> PlaneCapabilities {
    PlaneCapabilities {
        id,
        possible_crtcs: crtcs,
        plane_type,
        formats: formats.to_vec(),
        ..PlaneCapabilities::default()
    }
}

#[test]
fn from_capabilities_synthesizes_linear_when_in_formats_is_absent() {
    let registry = PlaneRegistry::from_capabilities(vec![plane(
        31,
        PlaneType::Primary,
        0b1,
        &[fourcc::XRGB8888, fourcc::ARGB8888],
    )]);

    let plane = registry.by_id(31).expect("plane 31");
    assert_eq!(
        plane.format_table.len(),
        2,
        "each advertised format gets a LINEAR entry"
    );
    assert!(
        plane
            .format_table
            .supports(fourcc::XRGB8888, Modifier::LINEAR)
    );
    assert!(!plane.format_table.supports(fourcc::NV12, Modifier::LINEAR));
    assert_eq!(plane.distinct_modifier_count(), 1, "just LINEAR");
}

#[test]
fn from_capabilities_keeps_an_explicit_format_table() {
    let afbc = Modifier(mod_code(vendor::ARM, 1)); // AFBC(16x16)
    let mut caps = plane(31, PlaneType::Overlay, 0b1, &[fourcc::XRGB8888]);
    caps.format_table = FormatTable::from_pairs([
        (fourcc::XRGB8888, Modifier::LINEAR),
        (fourcc::XRGB8888, afbc),
    ]);
    caps.has_format_modifiers = true;

    let registry = PlaneRegistry::from_capabilities(vec![caps]);
    let plane = registry.by_id(31).expect("plane 31");

    assert_eq!(plane.format_table.len(), 2, "the explicit table is kept");
    assert!(plane.format_table.supports(fourcc::XRGB8888, afbc));
    assert_eq!(plane.distinct_modifier_count(), 2);
}

#[test]
fn bandwidth_class_is_precomputed_and_falls_back_for_unknown_modifiers() {
    // `mod_code(ARM, 1)` is AFBC(16x16), *not* the legacy interleaved layout --
    // the trap drmkit-fmt names a constant for. Cover both so the distinction
    // stays visible here too.
    let afbc = Modifier(mod_code(vendor::ARM, 1));
    let interleaved = ARM_16X16_BLOCK_U_INTERLEAVED;
    assert_ne!(afbc, interleaved);

    let mut caps = plane(31, PlaneType::Overlay, 0b1, &[]);
    caps.format_table = FormatTable::from_pairs([
        (fourcc::XRGB8888, Modifier::LINEAR),
        (fourcc::XRGB8888, afbc),
        (fourcc::XRGB8888, interleaved),
    ]);

    let registry = PlaneRegistry::from_capabilities(vec![caps]);
    let plane = registry.by_id(31).expect("plane 31");

    assert_eq!(plane.bandwidth_class(0), BandwidthClass::Linear);
    assert_eq!(plane.bandwidth_class(afbc.0), BandwidthClass::Compression);
    assert_eq!(plane.bandwidth_class(interleaved.0), BandwidthClass::Tiling);

    // Not advertised: decoded live rather than guessed.
    let qcom = Modifier(mod_code(vendor::QCOM, 1));
    assert_eq!(
        plane.bandwidth_class(qcom.0),
        BandwidthClass::Compression,
        "an unadvertised modifier is classified on the spot"
    );
}

#[test]
fn for_crtc_filters_by_the_possible_crtcs_mask() {
    let registry = PlaneRegistry::from_capabilities(vec![
        plane(31, PlaneType::Primary, 0b001, &[fourcc::XRGB8888]),
        plane(32, PlaneType::Overlay, 0b011, &[fourcc::XRGB8888]),
        plane(33, PlaneType::Overlay, 0b110, &[fourcc::XRGB8888]),
    ]);

    let ids = |crtc| -> Vec<u32> { registry.for_crtc(crtc).map(|p| p.id).collect() };
    assert_eq!(ids(0), vec![31, 32]);
    assert_eq!(ids(1), vec![32, 33]);
    assert_eq!(ids(2), vec![33]);
    assert!(ids(3).is_empty());
}

/// A CRTC index past the width of the mask must not shift out of range.
#[test]
fn compatible_with_crtc_rejects_out_of_range_indices() {
    let caps = plane(31, PlaneType::Primary, u32::MAX, &[]);
    assert!(caps.compatible_with_crtc(31));
    assert!(
        !caps.compatible_with_crtc(32),
        "index 32 has no bit in a 32-bit mask; shifting by it would be UB in C"
    );
    assert!(!caps.compatible_with_crtc(u32::MAX));
}

#[test]
fn registry_lookups() {
    let registry = PlaneRegistry::from_capabilities(vec![
        plane(31, PlaneType::Primary, 0b1, &[fourcc::XRGB8888]),
        plane(32, PlaneType::Cursor, 0b1, &[fourcc::ARGB8888]),
    ]);

    assert_eq!(registry.len(), 2);
    assert!(!registry.is_empty());
    assert_eq!(
        registry.by_id(32).map(|p| p.plane_type),
        Some(PlaneType::Cursor)
    );
    assert!(registry.by_id(99).is_none());
    assert!(PlaneRegistry::new().is_empty());
}

#[test]
fn supports_format_consults_the_bare_format_list() {
    let caps = plane(
        31,
        PlaneType::Primary,
        0b1,
        &[fourcc::XRGB8888, fourcc::NV12],
    );
    assert!(caps.supports_format(fourcc::XRGB8888));
    assert!(caps.supports_format(fourcc::NV12));
    assert!(!caps.supports_format(fourcc::ARGB8888));
}

// --- bipartite matcher -------------------------------------------------------

/// Port of the `test_matching.cpp` shape: a small graph with a known answer.
#[test]
fn matcher_finds_a_perfect_matching() {
    let mut matching = BipartiteMatching::with_shape(3, 3);
    matching.add_edge(0, 0);
    matching.add_edge(0, 1);
    matching.add_edge(1, 1);
    matching.add_edge(1, 2);
    matching.add_edge(2, 2);

    assert_eq!(matching.solve(), 3);
    for u in 0..3 {
        assert!(matching.match_for_left(u).is_some());
    }
}

/// Two layers competing for one plane: exactly one is placed.
#[test]
fn matcher_handles_contention() {
    let mut matching = BipartiteMatching::with_shape(2, 1);
    matching.add_edge(0, 0);
    matching.add_edge(1, 0);

    assert_eq!(matching.solve(), 1);
    let placed = usize::from(matching.match_for_left(0).is_some())
        + usize::from(matching.match_for_left(1).is_some());
    assert_eq!(placed, 1);
    assert!(matching.match_for_right(0).is_some());
}

#[test]
fn matcher_with_no_edges_places_nothing() {
    let mut matching = BipartiteMatching::with_shape(4, 4);
    assert_eq!(matching.solve(), 0);
    assert_eq!(matching.match_for_left(0), None);
    assert_eq!(matching.matched_count(), 0);
}

/// A higher score is tried first, so when both planes are free the preferred
/// one wins. This is the mechanism the allocator's stability bonus rides on.
#[test]
fn matcher_prefers_the_higher_scored_plane() {
    let mut matching = BipartiteMatching::with_shape(1, 2);
    matching.add_scored_edge(0, 0, 10);
    matching.add_scored_edge(0, 1, 100);

    assert_eq!(matching.solve(), 1);
    assert_eq!(
        matching.match_for_left(0),
        Some(1),
        "the higher-scored plane should win when both are free"
    );
}

/// But preference must never cost a placement: when the preferred plane is
/// the only option for another layer, cardinality still wins.
#[test]
fn matcher_gives_up_a_preference_to_keep_cardinality() {
    // Layer 0 prefers plane 0 strongly but can also use plane 1.
    // Layer 1 can only use plane 0.
    let mut matching = BipartiteMatching::with_shape(2, 2);
    matching.add_scored_edge(0, 0, 1000);
    matching.add_scored_edge(0, 1, 0);
    matching.add_scored_edge(1, 0, 0);

    assert_eq!(
        matching.solve(),
        2,
        "both layers must be placed even though it costs layer 0 its preference"
    );
    assert_eq!(matching.match_for_left(1), Some(0));
    assert_eq!(matching.match_for_left(0), Some(1));
}

/// Out-of-range endpoints are ignored, so a caller filtering candidates need
/// not re-check bounds the matcher already knows.
#[test]
fn matcher_ignores_out_of_range_edges() {
    let mut matching = BipartiteMatching::with_shape(2, 2);
    matching.add_edge(0, 0);
    matching.add_edge(5, 0); // no such layer
    matching.add_edge(0, 9); // no such plane

    assert_eq!(matching.solve(), 1);
    assert_eq!(matching.match_for_left(0), Some(0));
    assert_eq!(matching.match_for_left(5), None);
}

/// Reset re-shapes and clears, and the matcher is reusable across frames.
#[test]
fn matcher_reset_reshapes_and_clears() {
    let mut matching = BipartiteMatching::with_shape(2, 2);
    matching.add_edge(0, 0);
    matching.add_edge(1, 1);
    assert_eq!(matching.solve(), 2);

    matching.reset(1, 3);
    assert_eq!(matching.left_len(), 1);
    assert_eq!(matching.right_len(), 3);
    assert_eq!(
        matching.solve(),
        0,
        "edges from the previous shape are gone"
    );

    matching.add_edge(0, 2);
    assert_eq!(matching.solve(), 1);
    assert_eq!(matching.match_for_left(0), Some(2));
}

// --- allocator scoring -------------------------------------------------------

/// A plane that accepts XRGB8888 LINEAR on CRTC 0.
fn scoring_plane(id: u32, plane_type: PlaneType) -> PlaneCapabilities {
    let mut caps = plane(id, plane_type, 0b1, &[fourcc::XRGB8888]);
    caps.build_format_metadata();
    caps
}

/// A 1080p XRGB8888 layer at the origin.
fn scoring_layer() -> Layer {
    let mut layer = Layer::new();
    layer
        .set_property(PropTag::PixelFormat, u64::from(fourcc::XRGB8888))
        .set_property(PropTag::CrtcW, 1920)
        .set_property(PropTag::CrtcH, 1080)
        .set_property(PropTag::SrcW, 1920 << 16)
        .set_property(PropTag::SrcH, 1080 << 16);
    layer
}

#[test]
fn bandwidth_class_bonus_ranks_compression_highest() {
    assert_eq!(bandwidth_class_bonus(BandwidthClass::Compression), 2);
    assert_eq!(bandwidth_class_bonus(BandwidthClass::Tiling), 1);
    assert_eq!(bandwidth_class_bonus(BandwidthClass::Linear), 0);
}

#[test]
fn cost_bias_buckets_by_frame_size() {
    const MB: u64 = 1024 * 1024;
    assert_eq!(cost_bias(0), 0);
    assert_eq!(cost_bias(MB - 1), 0);
    assert_eq!(cost_bias(MB), 1);
    assert_eq!(cost_bias(4 * MB), 2);
    assert_eq!(cost_bias(16 * MB), 3);
    assert_eq!(cost_bias(u64::MAX), 3, "bounded above");
}

#[test]
fn layer_priority_ranks_video_above_animated_above_static() {
    let mut video = Layer::new();
    video.set_content_type(ContentType::Video);
    assert_eq!(layer_priority(&video), 100);

    let mut fast = Layer::new();
    fast.set_update_hint(60);
    assert_eq!(layer_priority(&fast), 80);

    let mut slow = Layer::new();
    slow.set_update_hint(10);
    assert_eq!(layer_priority(&slow), 50);

    assert_eq!(layer_priority(&Layer::new()), 10, "static default");
}

/// Content class must dominate: no `app_priority` can lift a Generic layer
/// above a Video one.
#[test]
fn keep_priority_lets_content_class_dominate_app_priority() {
    let mut video = Layer::new();
    video
        .set_content_type(ContentType::Video)
        .set_app_priority(0);

    let mut generic = Layer::new();
    generic.set_app_priority(u8::MAX);

    assert!(
        keep_priority(&video) > keep_priority(&generic),
        "a maximum app_priority must not outrank a higher content class"
    );

    // Within a class, app_priority breaks the tie.
    let mut low = Layer::new();
    low.set_content_type(ContentType::Video).set_app_priority(1);
    assert!(keep_priority(&video) < keep_priority(&low));
}

/// The stability bonus must outweigh every structural term combined, so that
/// among equal-cardinality matchings the previous assignment wins.
#[test]
fn stability_bonus_outweighs_every_structural_term() {
    let plane = scoring_plane(31, PlaneType::Primary);
    let mut layer = scoring_layer();
    layer.set_content_type(ContentType::Video);
    layer.set_property(PropTag::Zpos, 0);

    let cold = score_pair(&plane, &layer, ScoreContext::default());
    let warm = score_pair(
        &plane,
        &layer,
        ScoreContext {
            held_last_frame: true,
            failure_hits: 0,
        },
    );

    assert_eq!(warm - cold, WARM_STABILITY_BONUS);
    assert!(
        WARM_STABILITY_BONUS > cold,
        "the bonus ({WARM_STABILITY_BONUS}) must exceed the best structural score ({cold}), \
         or a newly attractive plane could displace a stable placement"
    );
}

#[test]
fn failure_hits_reduce_the_score() {
    let plane = scoring_plane(31, PlaneType::Overlay);
    let layer = scoring_layer();

    let clean = score_pair(&plane, &layer, ScoreContext::default());
    let punished = score_pair(
        &plane,
        &layer,
        ScoreContext {
            held_last_frame: false,
            failure_hits: 5,
        },
    );
    assert_eq!(clean - punished, 5);
}

#[test]
fn composition_layer_prefers_the_primary_plane() {
    let primary = scoring_plane(31, PlaneType::Primary);
    let overlay = scoring_plane(32, PlaneType::Overlay);

    let mut layer = scoring_layer();
    layer.set_composition_layer(true);

    assert!(
        score_pair(&primary, &layer, ScoreContext::default())
            > score_pair(&overlay, &layer, ScoreContext::default()),
        "the composition output belongs on primary"
    );
}

/// A layer pinned to primary's minimum zpos is asking for direct scanout
/// there. Without the bonus the overlay preference wins and primary keeps
/// whatever the console left on it.
#[test]
fn layer_at_primary_min_zpos_prefers_primary() {
    let mut primary = scoring_plane(31, PlaneType::Primary);
    primary.zpos_min = Some(0);
    primary.zpos_max = Some(0);
    let mut overlay = scoring_plane(32, PlaneType::Overlay);
    overlay.zpos_min = Some(1);
    overlay.zpos_max = Some(4);

    let mut layer = scoring_layer();
    layer.set_property(PropTag::Zpos, 0);

    assert!(
        score_pair(&primary, &layer, ScoreContext::default())
            > score_pair(&overlay, &layer, ScoreContext::default())
    );
}

/// On platforms with no kernel zpos property, zpos 0 means primary and any
/// positive zpos means overlay.
#[test]
fn zpos_convention_without_a_kernel_zpos_property() {
    let primary = scoring_plane(31, PlaneType::Primary); // zpos_min: None
    let overlay = scoring_plane(32, PlaneType::Overlay);

    let mut bottom = scoring_layer();
    bottom.set_property(PropTag::Zpos, 0);
    assert!(
        score_pair(&primary, &bottom, ScoreContext::default())
            > score_pair(&overlay, &bottom, ScoreContext::default()),
        "zpos 0 means bottom-most, which is primary"
    );

    let mut above = scoring_layer();
    above.set_property(PropTag::Zpos, 1);
    assert!(
        score_pair(&overlay, &above, ScoreContext::default())
            > score_pair(&primary, &above, ScoreContext::default()),
        "a positive zpos means above primary, which is an overlay"
    );
}

// --- static compatibility ----------------------------------------------------

#[test]
fn statically_compatible_accepts_a_plain_layer() {
    let plane = scoring_plane(31, PlaneType::Overlay);
    let layer = scoring_layer();
    assert!(plane_statically_compatible(&plane, &layer, 0));
}

#[test]
fn a_layer_without_a_format_is_never_compatible() {
    let plane = scoring_plane(31, PlaneType::Overlay);
    let mut layer = Layer::new();
    layer
        .set_property(PropTag::CrtcW, 1920)
        .set_property(PropTag::CrtcH, 1080);

    assert!(
        !plane_statically_compatible(&plane, &layer, 0),
        "no format means no buffer this frame; placing it would report the \
         layer as both assigned and skipped"
    );
}

#[test]
fn composited_layers_are_incompatible_with_every_plane() {
    let plane = scoring_plane(31, PlaneType::Overlay);

    let mut forced = scoring_layer();
    forced.set_force_composited(true);
    assert!(!plane_statically_compatible(&plane, &forced, 0));

    let mut transient = scoring_layer();
    transient.set_transient_composited(true);
    assert!(!plane_statically_compatible(&plane, &transient, 0));
}

/// A plane must expose every requested rotation bit, not merely have the
/// property — otherwise a 90-degree request lands on 0/180-only hardware.
#[test]
fn rotation_requires_the_exact_bits() {
    let mut plane = scoring_plane(31, PlaneType::Overlay);
    plane.supports_rotation = true;
    plane.rotation_bits = 0b0001 | 0b0100; // rotate-0 and rotate-180 only

    let mut layer = scoring_layer();
    layer.set_property(PropTag::Rotation, 0b0001);
    assert!(plane_statically_compatible(&plane, &layer, 0));

    layer.set_property(PropTag::Rotation, 0b0010); // rotate-90
    assert!(
        !plane_statically_compatible(&plane, &layer, 0),
        "advertising a rotation property is not the same as implementing 90 degrees"
    );
}

#[test]
fn scaling_requires_a_scaling_plane() {
    let mut plane = scoring_plane(31, PlaneType::Overlay);
    plane.supports_scaling = false;

    let mut layer = scoring_layer();
    layer.set_property(PropTag::CrtcW, 1280); // 1920 source into 1280 dest
    assert!(layer.requires_scaling());
    assert!(!plane_statically_compatible(&plane, &layer, 0));

    plane.supports_scaling = true;
    assert!(plane_statically_compatible(&plane, &layer, 0));
}

/// A requested zpos outside the plane's range no longer rules the plane out:
/// the value written is the stacked one, which fits (drm-cxx `ad47fa9`). On
/// tidss, both planes `[0, 1]`, the old gate kept every layer above 1 off the
/// overlay.
#[test]
fn a_zpos_outside_the_planes_range_does_not_rule_it_out() {
    let mut plane = scoring_plane(31, PlaneType::Overlay);
    plane.zpos_min = Some(2);
    plane.zpos_max = Some(4);

    let mut layer = scoring_layer();
    for zpos in [1, 3, 5, 99] {
        layer.set_property(PropTag::Zpos, zpos);
        assert!(
            plane_statically_compatible(&plane, &layer, 0),
            "zpos {zpos}"
        );
    }
}

#[test]
fn cursor_planes_enforce_their_maximum_size() {
    let mut plane = scoring_plane(31, PlaneType::Cursor);
    plane.cursor_max_w = 64;
    plane.cursor_max_h = 64;

    let mut layer = scoring_layer();
    layer
        .set_property(PropTag::CrtcW, 64)
        .set_property(PropTag::CrtcH, 64);
    layer
        .set_property(PropTag::SrcW, 64 << 16)
        .set_property(PropTag::SrcH, 64 << 16);
    assert!(plane_statically_compatible(&plane, &layer, 0));

    layer
        .set_property(PropTag::CrtcW, 128)
        .set_property(PropTag::SrcW, 128 << 16);
    assert!(!plane_statically_compatible(&plane, &layer, 0));
}

#[test]
fn a_wrong_crtc_is_incompatible() {
    let plane = scoring_plane(31, PlaneType::Overlay); // CRTC 0 only
    let layer = scoring_layer();
    assert!(plane_statically_compatible(&plane, &layer, 0));
    assert!(!plane_statically_compatible(&plane, &layer, 1));
}

// --- spatial splitting -------------------------------------------------------

#[expect(
    clippy::cast_sign_loss,
    reason = "KMS carries CRTC_X/Y as signed values in u64 property slots; the \
              round trip through the property bag is exact"
)]
fn at(x: i32, y: i32, w: u32, h: u32) -> Layer {
    let mut layer = Layer::new();
    layer
        .set_property(PropTag::CrtcX, x as u64)
        .set_property(PropTag::CrtcY, y as u64)
        .set_property(PropTag::CrtcW, u64::from(w))
        .set_property(PropTag::CrtcH, u64::from(h));
    layer
}

#[test]
fn rects_intersect_is_half_open() {
    let a = Rect {
        x: 0,
        y: 0,
        w: 10,
        h: 10,
    };
    assert!(rects_intersect(
        a,
        Rect {
            x: 9,
            y: 9,
            w: 10,
            h: 10
        }
    ));
    assert!(
        !rects_intersect(
            a,
            Rect {
                x: 10,
                y: 0,
                w: 10,
                h: 10
            }
        ),
        "edge-adjacent rectangles do not overlap"
    );
    assert!(!rects_intersect(
        a,
        Rect {
            x: 0,
            y: 10,
            w: 10,
            h: 10
        }
    ));
    // A zero-area rectangle *inside* another reports as intersecting. Strictly
    // this is wrong for half-open intervals -- [5,5) is empty and overlaps
    // nothing -- but it is what the C++ predicate computes, and it is kept for
    // parity. The consequence is nil: the predicate only partitions the search,
    // never decides correctness, so a degenerate layer merely joins a group it
    // did not need to. Not worth diverging over.
    assert!(rects_intersect(
        a,
        Rect {
            x: 5,
            y: 5,
            w: 0,
            h: 0
        }
    ));
}

/// Computed in `i64`, so a rectangle near `i32::MAX` cannot wrap into a false
/// negative the way 32-bit arithmetic would.
#[test]
fn rects_intersect_does_not_wrap_near_the_coordinate_limit() {
    let a = Rect {
        x: i32::MAX - 5,
        y: 0,
        w: 100,
        h: 10,
    };
    let b = Rect {
        x: i32::MAX - 1,
        y: 0,
        w: 100,
        h: 10,
    };
    assert!(rects_intersect(a, b));
}

#[test]
fn split_separates_non_overlapping_layers() {
    let left = at(0, 0, 100, 100);
    let right = at(200, 0, 100, 100);
    let layers = [&left, &right];

    let groups = split_independent_groups(&layers);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0], vec![0]);
    assert_eq!(groups[1], vec![1]);
}

#[test]
fn split_joins_overlapping_layers() {
    let a = at(0, 0, 100, 100);
    let b = at(50, 50, 100, 100);
    let c = at(500, 500, 10, 10);
    let layers = [&a, &b, &c];

    let groups = split_independent_groups(&layers);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0], vec![0, 1], "a and b overlap");
    assert_eq!(groups[1], vec![2]);
}

/// Overlap is transitive through the union-find: a-b and b-c overlapping puts
/// all three together even though a and c do not touch.
#[test]
fn split_is_transitive() {
    let a = at(0, 0, 100, 100);
    let b = at(90, 0, 100, 100);
    let c = at(180, 0, 100, 100);
    let layers = [&a, &b, &c];

    let groups = split_independent_groups(&layers);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0], vec![0, 1, 2]);
}

/// With equal priorities, group order follows the caller's layer order, every
/// time -- the caller consumes a shared plane pool, so order decides who wins
/// contention. Upstream's came out of an `unordered_map` until drm-cxx#236.
#[test]
fn split_group_order_is_deterministic() {
    let a = at(300, 0, 50, 50);
    let b = at(0, 0, 50, 50);
    let c = at(150, 0, 50, 50);
    let layers = [&a, &b, &c];

    for _ in 0..32 {
        let groups = split_independent_groups(&layers);
        assert_eq!(
            groups,
            vec![vec![0], vec![1], vec![2]],
            "groups must come back ordered by lowest member index, every time"
        );
    }
}

#[test]
fn split_handles_empty_and_single_inputs() {
    assert!(split_independent_groups(&[]).is_empty());
    let only = at(0, 0, 10, 10);
    assert_eq!(split_independent_groups(&[&only]), vec![vec![0]]);
}

/// The layer-level half of drm-cxx#236: a lone high-priority layer's group
/// goes ahead of groups listed before it, so it gets a contested plane first.
#[test]
fn split_puts_the_highest_priority_group_first() {
    let low = at(0, 0, 50, 50);
    let also_low = at(100, 0, 50, 50);
    let mut high = at(200, 0, 50, 50);
    high.set_app_priority(200);
    let layers = [&low, &also_low, &high];

    assert_eq!(
        split_independent_groups(&layers),
        vec![vec![2], vec![0], vec![1]]
    );
}

/// `test_layer_groups.cpp`, on plain values: `overlaps` joins the listed pairs.
fn overlaps(edges: &[(i32, i32)]) -> impl Fn(&i32, &i32) -> bool + '_ {
    move |a, b| {
        edges
            .iter()
            .any(|&(x, y)| (x, y) == (*a, *b) || (y, x) == (*a, *b))
    }
}

/// Map indices back to the values, which is what upstream's cases compare.
fn values(items: &[i32], groups: &[Vec<usize>]) -> Vec<Vec<i32>> {
    groups
        .iter()
        .map(|group| group.iter().map(|&i| items[i]).collect())
        .collect()
}

/// `LayerGroups.OverlapChainsFormOneGroup`
#[test]
fn layer_groups_overlap_chains_form_one_group() {
    let items = [0, 1, 2, 3];
    let groups = independent_groups(&items, overlaps(&[(0, 2), (2, 3)]), |_| 0);
    assert_eq!(values(&items, &groups), vec![vec![0, 2, 3], vec![1]]);
}

/// `LayerGroups.TiesKeepInputOrder`
#[test]
fn layer_groups_ties_keep_input_order() {
    let items = [5, 4, 3, 2, 1, 0];
    let groups = independent_groups(&items, overlaps(&[(4, 0)]), |_| 0);
    assert_eq!(
        values(&items, &groups),
        vec![vec![5], vec![4, 0], vec![3], vec![2], vec![1]]
    );
}

/// `LayerGroups.HighestPriorityGroupFirst`: 3 is the video layer, and 1 and 3
/// overlap.
#[test]
fn layer_groups_highest_priority_group_first() {
    let items = [0, 1, 2, 3];
    let groups = independent_groups(&items, overlaps(&[(1, 3)]), |&item| {
        if item == 3 { 100 } else { 10 }
    });
    assert_eq!(values(&items, &groups), vec![vec![1, 3], vec![0], vec![2]]);
}

/// `LayerGroups.EmptyInput`
#[test]
fn layer_groups_empty_input() {
    let items: [i32; 0] = [];
    assert!(independent_groups(&items, overlaps(&[]), |_| 0).is_empty());
}

// --- Output ------------------------------------------------------------------

/// `OutputTest.ConstructWithCompositionLayer`
#[test]
fn output_designates_a_composition_layer() {
    let mut output = Output::new(42);
    assert_eq!(output.crtc_id(), 42);
    assert!(output.is_empty());
    assert_eq!(output.composition_layer(), None);

    let canvas = output.add_layer();
    assert!(output.set_composition_layer(canvas));

    assert_eq!(output.composition_layer(), Some(canvas));
    assert!(output.layer(canvas).expect("canvas").is_composition_layer());
}

/// `OutputTest.AddAndRemoveLayer`
#[test]
fn output_adds_and_removes_layers() {
    let mut output = Output::new(1);
    let a = output.add_layer();
    let b = output.add_layer();
    assert_eq!(output.len(), 2);

    assert!(output.remove_layer(a));
    assert_eq!(output.len(), 1);
    assert!(output.layer(a).is_none());
    assert!(output.layer(b).is_some());

    assert!(
        !output.remove_layer(a),
        "removing twice is not an error, just false"
    );
}

/// Ids are never reused, which is what lets the allocator key warm-start state
/// on them: a recycled id could make a new layer masquerade as an old one.
#[test]
fn output_never_reuses_layer_ids() {
    let mut output = Output::new(1);
    let first = output.add_layer();
    output.remove_layer(first);
    let second = output.add_layer();

    assert_ne!(first, second, "a removed id must not come back");
}

/// Removing the composition layer leaves the output without one, rather than
/// pointing at a layer that no longer exists.
#[test]
fn removing_the_composition_layer_clears_the_designation() {
    let mut output = Output::new(1);
    let canvas = output.add_layer();
    output.set_composition_layer(canvas);

    output.remove_layer(canvas);
    assert_eq!(output.composition_layer(), None);
}

/// `OutputTest.SetCompositionLayerUpdatesFlag`
///
/// Exactly one layer carries the flag: designating a new one clears the old.
#[test]
fn set_composition_layer_moves_the_flag() {
    let mut output = Output::new(1);
    let first = output.add_layer();
    let second = output.add_layer();

    output.set_composition_layer(first);
    assert!(output.layer(first).unwrap().is_composition_layer());

    output.set_composition_layer(second);
    assert!(
        !output.layer(first).unwrap().is_composition_layer(),
        "the previous composition layer must lose the flag"
    );
    assert!(output.layer(second).unwrap().is_composition_layer());

    assert!(
        !output.set_composition_layer(LayerId(999)),
        "a layer this output does not own is rejected"
    );
}

/// `OutputTest.AnyLayerDirtyDetection` and `ChangedLayersReturnsOnlyDirty`
#[test]
fn output_tracks_dirty_layers() {
    let mut output = Output::new(1);
    let a = output.add_layer();
    let b = output.add_layer();
    assert!(output.any_layer_dirty(), "freshly added layers are dirty");

    output.mark_clean();
    assert!(!output.any_layer_dirty());
    assert_eq!(output.changed_layers().count(), 0);

    output.layer_mut(b).unwrap().set_property(PropTag::FbId, 7);
    assert!(output.any_layer_dirty());

    let changed: Vec<_> = output.changed_layers().map(|entry| entry.id).collect();
    assert_eq!(changed, vec![b], "only the layer that changed");
    assert!(output.layer(a).unwrap().property(PropTag::FbId).is_none());
}

/// `OutputTest.SortLayersByZpos`
#[test]
fn output_sorts_layers_by_zpos() {
    let mut output = Output::new(1);
    let top = output.add_layer();
    let bottom = output.add_layer();
    let middle = output.add_layer();

    output
        .layer_mut(top)
        .unwrap()
        .set_property(PropTag::Zpos, 9);
    output
        .layer_mut(bottom)
        .unwrap()
        .set_property(PropTag::Zpos, 0);
    output
        .layer_mut(middle)
        .unwrap()
        .set_property(PropTag::Zpos, 4);

    output.sort_layers_by_zpos();

    let order: Vec<_> = output.layers().map(|entry| entry.id).collect();
    assert_eq!(order, vec![bottom, middle, top]);
}

/// An unset zpos sorts as 0, and the sort is stable so equal values keep the
/// order they were added in.
#[test]
fn zpos_sort_treats_unset_as_zero_and_is_stable() {
    let mut output = Output::new(1);
    let first_unset = output.add_layer();
    let second_unset = output.add_layer();
    let above = output.add_layer();
    output
        .layer_mut(above)
        .unwrap()
        .set_property(PropTag::Zpos, 1);

    output.sort_layers_by_zpos();

    let order: Vec<_> = output.layers().map(|entry| entry.id).collect();
    assert_eq!(
        order,
        vec![first_unset, second_unset, above],
        "unset sorts as 0, and equal values keep insertion order"
    );
}

/// `TestCacheTest.SuccessesAreNotCountedAsFailures` (drm-cxx#267).
///
/// A plane the kernel keeps accepting must carry no penalty, however often it
/// is used. Upstream measured the alternative on a display with one usable
/// primary out of five: the accepted plane was chosen ten times and then never
/// again while the search cycled the overlays, every one rejected.
#[test]
fn test_cache_does_not_count_successes_as_failures() {
    let mut cache = TestCache::new();
    for _ in 0..5 {
        cache.record(97, 0x1234, true);
    }
    assert_eq!(cache.failure_count(97, 0x1234), 0);
    assert_eq!(cache.lookup(97, 0x1234), Some(true));
}

/// `TestCacheTest.FailuresAccumulate`
#[test]
fn test_cache_failures_accumulate() {
    let mut cache = TestCache::new();
    cache.record(100, 0x1234, false);
    assert_eq!(cache.failure_count(100, 0x1234), 1);
    cache.record(100, 0x1234, false);
    assert_eq!(cache.failure_count(100, 0x1234), 2);
    assert_eq!(cache.lookup(100, 0x1234), Some(false));
}

/// `TestCacheTest.LatestVerdictWins`: a combination rejected while some other
/// state was wrong stops being penalized once it succeeds.
#[test]
fn test_cache_latest_verdict_wins() {
    let mut cache = TestCache::new();
    cache.record(97, 0x1234, false);
    cache.record(97, 0x1234, false);
    assert_eq!(cache.failure_count(97, 0x1234), 2);
    cache.record(97, 0x1234, true);
    assert_eq!(cache.failure_count(97, 0x1234), 0);
    assert_eq!(cache.lookup(97, 0x1234), Some(true));
}

/// `TestCacheTest.UnknownIsNotAFailure`
#[test]
fn test_cache_unknown_is_not_a_failure() {
    let cache = TestCache::new();
    assert_eq!(cache.failure_count(999, 0x1), 0);
    assert_eq!(cache.lookup(999, 0x1), None);
}

// --- Plane alpha (`test_plane_alpha.cpp`, drm-cxx#265) ----------------------

const ALPHA_FULL: u64 = 0xFFFF;

/// `PlaneAlphaTest.FullRangeIsIdentity`
#[test]
fn plane_alpha_full_range_is_identity() {
    for value in [0, 0x4000, 0x8000, ALPHA_FULL] {
        assert_eq!(rescale_alpha(value, ALPHA_FULL), value);
    }
}

/// `PlaneAlphaTest.FullOpacityMapsOntoAdvertisedMaximum`
#[test]
fn plane_alpha_full_opacity_maps_onto_the_advertised_maximum() {
    assert_eq!(rescale_alpha(ALPHA_FULL, 255), 255);
    assert_eq!(rescale_alpha(ALPHA_FULL, 1023), 1023);
    assert_eq!(rescale_alpha(ALPHA_FULL, 1), 1);
}

/// `PlaneAlphaTest.PartialAlphaKeepsItsFraction`: the reason this rescales
/// rather than clamps.
#[test]
fn plane_alpha_partial_alpha_keeps_its_fraction() {
    assert_eq!(rescale_alpha(0x8000, 255), 128);
    assert_eq!(rescale_alpha(0x4000, 255), 64);
    assert_eq!(rescale_alpha(0x8000, 1023), 512);
    assert_eq!(rescale_alpha(0x4000, 1023), 256);
}

/// `PlaneAlphaTest.ZeroStaysZero`
#[test]
fn plane_alpha_zero_stays_zero() {
    for max in [255, 1023, 1] {
        assert_eq!(rescale_alpha(0, max), 0);
    }
}

/// `PlaneAlphaTest.OversizedInputSaturatesRatherThanOverflowing`
#[test]
fn plane_alpha_oversized_input_saturates() {
    assert_eq!(rescale_alpha(0x1_0000, 255), 255);
    assert_eq!(rescale_alpha(0xFFFF_FFFF, 255), 255);
    assert_eq!(rescale_alpha(0xFFFF_FFFF, ALPHA_FULL), ALPHA_FULL);
}

/// `PlaneAlphaTest.MonotonicNonDecreasing`
#[test]
fn plane_alpha_is_monotonic_and_bounded() {
    for max in [1, 15, 255, 1023, 4095] {
        let mut previous = 0;
        for value in (0..=ALPHA_FULL).step_by(97) {
            let got = rescale_alpha(value, max);
            assert!(got >= previous, "max={max} value={value}");
            assert!(got <= max, "max={max} value={value}");
            previous = got;
        }
    }
}

/// `PlaneAlphaTest.RespectsEachPlanesOwnAdvertisedRange`
#[test]
fn plane_alpha_respects_each_planes_own_range() {
    assert_eq!(rescale_alpha(ALPHA_FULL, 0x7FFF), 0x7FFF);
    assert_eq!(rescale_alpha(0x8000, 0x7FFF), 0x4000);
    assert_eq!(rescale_alpha(ALPHA_FULL / 2, 1), 0);
    assert_eq!(rescale_alpha(ALPHA_FULL, 1), 1);
}

// --- zpos order (`test_zpos_order.cpp`) --------------------------------------

fn zplane(id: u32, plane_type: PlaneType, min: Option<u64>, max: Option<u64>) -> PlaneCapabilities {
    PlaneCapabilities {
        id,
        plane_type,
        zpos_min: min,
        zpos_max: max,
        ..PlaneCapabilities::default()
    }
}

/// i.MX LCDIF / vc4: primary pinned at 0.
fn primary_0() -> PlaneCapabilities {
    zplane(31, PlaneType::Primary, Some(0), Some(0))
}
/// amdgpu DC: primary pinned at 2.
fn primary_2() -> PlaneCapabilities {
    zplane(40, PlaneType::Primary, Some(2), Some(2))
}
fn mutable_overlay() -> PlaneCapabilities {
    zplane(50, PlaneType::Overlay, Some(0), Some(255))
}
/// No zpos property at all (Tegra display).
fn no_zpos() -> PlaneCapabilities {
    zplane(60, PlaneType::Overlay, None, None)
}

/// `ZposOrder.FixedOnlyWhenMinEqualsMax`
#[test]
fn zpos_fixed_only_when_min_equals_max() {
    assert!(zpos_fixed(&primary_0()));
    assert!(zpos_fixed(&primary_2()));
    assert!(!zpos_fixed(&mutable_overlay()));
    assert!(!zpos_fixed(&no_zpos()));
}

/// `ZposOrder.EffectiveZposIsSlotOnFixedPlanesElseRequested`
#[test]
fn effective_zpos_is_the_slot_on_fixed_planes() {
    assert_eq!(effective_zpos(&primary_2(), Some(7)), Some(2));
    assert_eq!(effective_zpos(&mutable_overlay(), Some(7)), Some(7));
    assert_eq!(effective_zpos(&no_zpos(), Some(7)), Some(7));
    assert_eq!(effective_zpos(&mutable_overlay(), None), None);
}

/// `ZposOrder.BottomLayerOnFixedPrimaryKeepsOrder`: the bottom layer may sit
/// on the fixed primary whatever its zpos, as long as everything above lands
/// higher.
#[test]
fn the_bottom_layer_may_take_a_fixed_primary() {
    assert!(stacking_consistent(
        &primary_2(),
        Some(0),
        &mutable_overlay(),
        Some(3)
    ));
    assert!(stacking_consistent(
        &primary_0(),
        Some(1),
        &mutable_overlay(),
        Some(5)
    ));
}

/// `ZposOrder.InvertedStackRejected`: a TEST accepts it, so this must not.
#[test]
fn an_inverted_stack_is_rejected() {
    assert!(!stacking_consistent(
        &primary_2(),
        Some(5),
        &mutable_overlay(),
        Some(3)
    ));
    assert!(!stacking_consistent(
        &mutable_overlay(),
        Some(3),
        &primary_2(),
        Some(5)
    ));
}

/// `ZposOrder.EqualEffectiveSlotForDifferentRequestRejected`
#[test]
fn different_requests_on_one_slot_are_rejected() {
    assert!(!stacking_consistent(
        &primary_2(),
        Some(5),
        &mutable_overlay(),
        Some(2)
    ));
}

/// `ZposOrder.UnorderedRequestsAlwaysConsistent`
#[test]
fn unordered_requests_are_always_consistent() {
    assert!(stacking_consistent(
        &primary_2(),
        None,
        &mutable_overlay(),
        Some(3)
    ));
    assert!(stacking_consistent(
        &primary_2(),
        Some(4),
        &mutable_overlay(),
        Some(4)
    ));
    assert!(stacking_consistent(
        &no_zpos(),
        Some(9),
        &primary_0(),
        Some(1)
    ));
}

/// `ZposOrder.PreviouslyLegalAssignmentsUnaffected`: everything the old range
/// gate admitted stays admitted.
#[test]
fn previously_legal_assignments_are_unaffected() {
    let overlay = mutable_overlay();
    assert!(stacking_consistent(&overlay, Some(1), &overlay, Some(9)));
    assert!(stacking_consistent(&overlay, Some(9), &overlay, Some(1)));
    assert!(stacking_consistent(
        &primary_2(),
        Some(2),
        &overlay,
        Some(5)
    ));
    assert!(stacking_consistent(
        &primary_0(),
        Some(0),
        &overlay,
        Some(1)
    ));
}

/// SA8155P-like: every plane mutable over `[0, 10]`.
fn sde(id: u32) -> PlaneCapabilities {
    zplane(id, PlaneType::Overlay, Some(0), Some(10))
}

fn written(entries: &[StackEntry<'_>]) -> Vec<u64> {
    entries
        .iter()
        .map(|e| e.written.unwrap_or(u64::MAX))
        .collect()
}

/// `ZposOrder.StackPacksArmedPlanesDensely`: composited layers that took the
/// values in between no longer push the stack up.
#[test]
fn the_stack_packs_armed_planes_densely() {
    let (a, b, c) = (sde(97), sde(115), sde(118));
    let mut e = [
        StackEntry::new(&b, Some(8)),
        StackEntry::new(&a, Some(0)),
        StackEntry::new(&c, Some(5)),
    ];
    assert!(stack_zpos(&mut e));
    assert_eq!(written(&e), vec![2, 0, 1]);
}

/// `ZposOrder.StackKeepsFixedSlot` (amdgpu primary at 2).
#[test]
fn the_stack_keeps_a_fixed_slot() {
    let (primary, overlay) = (primary_2(), mutable_overlay());
    let mut e = [
        StackEntry::new(&primary, Some(2)),
        StackEntry::new(&overlay, Some(9)),
        StackEntry::new(&overlay, Some(1)),
    ];
    assert!(stack_zpos(&mut e));
    assert_eq!(written(&e), vec![2, 3, 0]);
}

/// `ZposOrder.StackSeparatesTies`, by plane id.
#[test]
fn the_stack_separates_ties() {
    let (b, c) = (sde(115), sde(118));
    let mut e = [StackEntry::new(&c, Some(4)), StackEntry::new(&b, Some(4))];
    assert!(stack_zpos(&mut e));
    assert_eq!(written(&e), vec![1, 0]);
}

/// `ZposOrder.StackHonorsPlaneMinimum`
#[test]
fn the_stack_honors_each_planes_minimum() {
    let (overlay, high) = (
        mutable_overlay(),
        zplane(70, PlaneType::Overlay, Some(4), Some(7)),
    );
    let mut e = [
        StackEntry::new(&overlay, Some(0)),
        StackEntry::new(&high, Some(6)),
    ];
    assert!(stack_zpos(&mut e));
    assert_eq!(written(&e), vec![0, 4]);
}

/// `ZposOrder.StackOverflowKeepsRequested`
#[test]
fn an_overflowing_stack_keeps_the_requested_values() {
    let (overlay, tight) = (
        mutable_overlay(),
        zplane(71, PlaneType::Overlay, Some(0), Some(0)),
    );
    let mut e = [
        StackEntry::new(&overlay, Some(0)),
        StackEntry::new(&tight, Some(5)),
    ];
    assert!(!stack_zpos(&mut e));
    assert_eq!(written(&e), vec![0, 5]);
    // A fixed slot at or below what sits under it.
    let primary = primary_0();
    let mut f = [
        StackEntry::new(&overlay, Some(0)),
        StackEntry::new(&overlay, Some(1)),
        StackEntry::new(&primary, Some(3)),
    ];
    assert!(!stack_zpos(&mut f));
}

/// `ZposOrder.StackSkipsUnrankable`
#[test]
fn the_stack_skips_what_it_cannot_rank() {
    let (none, overlay) = (no_zpos(), mutable_overlay());
    let mut e = [
        StackEntry::new(&none, Some(3)),
        StackEntry::new(&overlay, None),
        StackEntry::new(&overlay, Some(9)),
    ];
    assert!(stack_zpos(&mut e));
    assert_eq!(e[0].written, Some(3));
    assert_eq!(e[1].written, None);
    assert_eq!(e[2].written, Some(0));
}

// --- plane order (drm-cxx 03bc1d7) -------------------------------------------

use crate::plane_order::{Choice, PlaneOrder, Position, plane_order_consistent};

/// Five layers, three planes that take anything, a canvas anywhere: two
/// placed, and the canvas sits between the run and the layers above it.
#[test]
fn plane_order_leaves_a_canvas_plane_for_the_run() {
    let order = PlaneOrder::new(5, 3, |_, _| true);
    let hosts = |_: usize| true;
    let choice = order
        .choose(5, None, Some(&hosts), |_| 1)
        .expect("a split exists");
    assert_eq!(
        choice,
        Choice {
            run: 0..3,
            canvas: Some(0)
        }
    );
    assert_eq!(order.plane_of(3, &choice), Some(1));
    assert_eq!(order.plane_of(4, &choice), Some(2));
    assert_eq!(order.plane_of(1, &choice), None);
}

/// Without a canvas the run needs no plane of its own, so one more layer fits.
#[test]
fn plane_order_without_a_canvas_drops_the_run() {
    let order = PlaneOrder::new(5, 3, |_, _| true);
    let choice = order.choose(5, None, None, |_| 1).expect("a split exists");
    assert_eq!(choice.run.len(), 2);
    assert_eq!(choice.canvas, None);
}

/// A forced-composited layer has to be inside the run, whatever it costs.
#[test]
fn plane_order_runs_cover_the_forced_layers() {
    let order = PlaneOrder::new(4, 4, |i, _| i != 3);
    let hosts = |_: usize| true;
    let choice = order
        .choose(4, Some(&(1..2)), Some(&hosts), |i| {
            if i == 1 { 1000 } else { 1 }
        })
        .expect("a split exists");
    assert!(
        choice.run.contains(&1) && choice.run.contains(&3),
        "{choice:?}"
    );
}

/// No host between the halves: fewer placed rather than a canvas out of
/// order.
#[test]
fn plane_order_needs_a_host_between_the_halves() {
    let order = PlaneOrder::new(4, 3, |_, _| true);
    // Only the top plane hosts the canvas, so the run must reach the top.
    let hosts = |j: usize| j == 2;
    let choice = order
        .choose(4, None, Some(&hosts), |_| 1)
        .expect("a split exists");
    assert_eq!(
        choice,
        Choice {
            run: 2..4,
            canvas: Some(2)
        }
    );
}

const fn stacked_at(zpos: u64, plane: Option<u32>, on_canvas: bool) -> Position {
    Position {
        zpos,
        plane,
        on_canvas,
    }
}

#[test]
fn plane_order_consistency() {
    // In order, with the run on a canvas in between.
    assert!(plane_order_consistent(&[
        stacked_at(0, Some(31), false),
        stacked_at(1, Some(32), true),
        stacked_at(2, Some(32), true),
        stacked_at(3, Some(33), false),
    ]));
    // Restacked: the cached planes now invert it.
    assert!(!plane_order_consistent(&[
        stacked_at(1, Some(31), false),
        stacked_at(0, Some(32), false),
    ]));
    // Two different zpos on one plane only work inside the canvas.
    assert!(!plane_order_consistent(&[
        stacked_at(0, Some(32), false),
        stacked_at(1, Some(32), true),
    ]));
    // Composited with nowhere to stack it.
    assert!(!plane_order_consistent(&[
        stacked_at(0, None, true),
        stacked_at(1, Some(32), false),
    ]));
    // Equal zpos ask for no order.
    assert!(plane_order_consistent(&[
        stacked_at(1, Some(33), false),
        stacked_at(1, Some(31), false),
    ]));
}
