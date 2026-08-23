// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parity port of the `test_layer.cpp` property and hashing cases from
//! drm-cxx @ `dc2915b`, plus the registry fixtures the allocator suite is
//! built on.

use super::*;
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

/// A disabled layer hashes as empty regardless of what it held before.
#[test]
fn disable_clears_every_property() {
    let mut layer = placed_layer();
    layer.set_assigned_plane(Some(31));
    layer.disable();

    assert_eq!(layer.property_count(), 0);
    assert_eq!(layer.property(PropTag::FbId), None);
    assert_eq!(layer.assigned_plane(), None);
    assert_eq!(layer.property_hash(), Layer::new().property_hash());
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
