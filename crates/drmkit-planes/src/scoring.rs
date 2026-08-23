// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Placement scoring and static compatibility.
//!
//! Port of the pure decision functions from `src/planes/allocator.cpp`. None of
//! these touch a device: they are the part of the allocator that can be tested
//! exhaustively against synthetic fixtures.

use drmkit_fmt::{BandwidthClass, Modifier, layer_fits_plane, scanout_cost_bytes};

use crate::layer::{ContentType, Layer, Rect};
use crate::prop::PropTag;
use crate::registry::{PlaneCapabilities, PlaneType};

/// The default worst-case compression ratio the cost model scores with.
const COMPRESSED_RATIO: f32 = drmkit_fmt::DEFAULT_COMPRESSED_RATIO;

/// Power-aware placement bonus for a buffer's bandwidth class.
///
/// Keeps compressed and tiled layers on planes — compositing one costs a GPU
/// decompress — and lets the cheap `LINEAR` ones composite when planes are
/// contested. A small tiebreak below the structural scores.
#[must_use]
pub const fn bandwidth_class_bonus(class: BandwidthClass) -> i32 {
    match class {
        // DCC / AFBC / UBWC: the biggest bandwidth and GPU-decompress saving.
        BandwidthClass::Compression => 2,
        // Better DRAM locality, same byte count.
        BandwidthClass::Tiling => 1,
        BandwidthClass::Linear => 0,
    }
}

/// Bounded tiebreak by a layer's per-frame scanout byte cost.
///
/// A layer that moves more bytes is costlier to demote to composition, which
/// re-reads both the source and the canvas. Bucketed to 0..=3 so it stays below
/// the structural scores.
#[must_use]
pub const fn cost_bias(scanout_bytes: u64) -> i32 {
    const MB: u64 = 1024 * 1024;
    if scanout_bytes >= 16 * MB {
        3 // ~4K RGBA and up
    } else if scanout_bytes >= 4 * MB {
        2 // ~1080p RGBA
    } else if scanout_bytes >= MB {
        1 // small overlay
    } else {
        0
    }
}

/// Content-class placement priority.
#[must_use]
pub fn layer_priority(layer: &Layer) -> i32 {
    if layer.content_type() == ContentType::Video {
        return 100;
    }
    if layer.update_hz() > 30 {
        return 80;
    }
    if layer.update_hz() > 0 {
        return 50;
    }
    10
}

/// Keep-or-drop ordering under plane pressure.
///
/// Content class dominates; the application's own priority only breaks ties
/// within a class. `layer_priority` is scaled above the `u8` range so a higher
/// content class always outranks any `app_priority`.
#[must_use]
pub fn keep_priority(layer: &Layer) -> i32 {
    layer_priority(layer) * 256 + i32::from(layer.app_priority())
}

/// The bonus for keeping a layer on the plane it held last frame.
///
/// Deliberately larger than the sum of every structural term below it, so that
/// **among equal-cardinality matchings** the previous assignment wins.
///
/// This is the v2.0.1 warm-start stability bonus (plan §4.6). It is safe
/// precisely because the matcher maximizes cardinality first and uses score
/// only to order tie-breaks: it can never place fewer layers, and never
/// overrides validity — [`plane_statically_compatible`] gates the edges and the
/// `TEST_ONLY` commit is the arbiter.
///
/// Without it, any change to the layer *set* — a compositor inserting a UI
/// backing-store slice while the platform-view overlays stay put — drops
/// warm-start, makes the full search re-solve from scratch, and reshuffles
/// already-placed layers onto different planes. Every overlay reprograms in one
/// commit, which is seen as flicker.
pub const WARM_STABILITY_BONUS: i32 = 100;

/// Everything `score_pair` needs that is not the plane or the layer.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScoreContext {
    /// Whether the layer held this plane last frame.
    pub held_last_frame: bool,
    /// How many times this `(plane, property_hash)` pair has failed a test
    /// commit. Subtracted from the score.
    pub failure_hits: u32,
}

/// Score a candidate placement. Higher is more preferred.
///
/// Score orders the search only — it can never change how many layers get
/// placed. See [`WARM_STABILITY_BONUS`].
#[must_use]
pub fn score_pair(plane: &PlaneCapabilities, layer: &Layer, context: ScoreContext) -> i32 {
    let mut score = 0;

    // A plane advertising the layer's exact (format, modifier) is strictly
    // preferable to one that supports the format only via linear fallback.
    if let Some(fourcc) = layer.format()
        && layer_fits_plane(&plane.format_table, fourcc, Modifier(layer.modifier()))
    {
        score += 4;
    }

    let is_composition = layer.is_composition_layer();

    // A composition layer strongly prefers the primary plane.
    if is_composition && plane.plane_type == PlaneType::Primary {
        score += 8;
    }

    let zpos = layer.property(PropTag::Zpos);

    // A non-composition layer whose zpos matches primary's minimum slot is
    // explicitly asking for direct scanout on primary -- the bottommost
    // toolkit layer, with zpos pinned to primary.zpos_min. Without this the
    // overlay bonus below wins, primary stays framebuffer-less, and whatever
    // was on it before (typically the fbcon console) stays on scanout.
    if !is_composition
        && plane.plane_type == PlaneType::Primary
        && let Some(z) = zpos
    {
        match plane.zpos_min {
            Some(min) if z == min => score += 10,
            // Platforms with no kernel-side zpos property (Tegra Orin display
            // among others) cannot honor per-plane zpos, so the caller
            // convention is zpos 0 == bottom-most == primary.
            None if z == 0 => score += 10,
            _ => {}
        }
    }

    // Non-composition layers otherwise prefer overlays.
    if !is_composition && plane.plane_type == PlaneType::Overlay {
        score += 2;
        // Pairs with the zpos-0-means-primary convention above: with no kernel
        // zpos and a positive request, overlay-as-natural-top applies, so bump
        // enough to beat primary's bare base score.
        if let Some(z) = zpos
            && plane.zpos_min.is_none()
            && z > 0
        {
            score += 8;
        }
    }

    // Bandwidth-aware bias: two nudges toward keeping the costliest layers on
    // planes when planes are contested. The class comes from the scanout
    // framebuffer's own modifier -- never a producer-internal compression
    // state, which would double-count a saving on a bus this allocator does
    // not manage.
    if let Some(fourcc) = layer.format() {
        let class = plane.bandwidth_class(layer.modifier());
        score += bandwidth_class_bonus(class);
        score += cost_bias(scanout_cost_bytes(
            layer.width(),
            layer.height(),
            fourcc,
            class,
            COMPRESSED_RATIO,
        ));
    }

    score += layer_priority(layer) / 10;

    if context.held_last_frame {
        score += WARM_STABILITY_BONUS;
    }

    // Penalize combinations a previous test commit rejected.
    score -= i32::try_from(context.failure_hits).unwrap_or(i32::MAX);

    score
}

/// Whether a plane could possibly scan out a layer.
///
/// **Necessary conditions only.** Passing does not mean the commit will
/// succeed — the atomic `TEST_ONLY` remains the arbiter. Failing means there is
/// no point asking.
#[must_use]
pub fn plane_statically_compatible(
    plane: &PlaneCapabilities,
    layer: &Layer,
    crtc_index: u32,
) -> bool {
    // A force-composited layer is incompatible with every plane. The caller --
    // a test rig, a debug overlay, an EGL Streams workaround -- has asked for
    // the composition fallback, and short-circuiting here propagates that
    // through every downstream path without each re-checking the flag.
    if layer.is_force_composited() || layer.is_transient_composited() {
        return false;
    }
    if !plane.compatible_with_crtc(crtc_index) {
        return false;
    }

    let Some(fourcc) = layer.format() else {
        // No format means no buffer this commit -- typically an EAGAIN skip
        // where the layer was never lowered. The permissive reading ("no
        // format fits anywhere") would phantom-place it: counted as assigned,
        // but with an empty property map nothing reaches the kernel. The
        // frame would then report the layer as both assigned and skipped,
        // breaking the commit report's accounting.
        return false;
    };

    // Honor the modifier: AFBC, DCC, and vendor tilings only land on planes
    // whose IN_FORMATS advertises the pair. An untagged layer falls through
    // the LINEAR/INVALID equivalence inside `layer_fits_plane`.
    if !layer_fits_plane(&plane.format_table, fourcc, Modifier(layer.modifier())) {
        return false;
    }

    // The plane must expose every requested rotate/reflect bit, not merely
    // have a rotation property: a plane can advertise rotation and implement
    // only 0/180 or reflect, silently dropping a 90/270 request.
    let rotation = layer.rotation();
    if rotation != 0 && plane.rotation_bits & rotation != rotation {
        return false;
    }

    if layer.requires_scaling() && !plane.supports_scaling {
        return false;
    }

    if let Some(z) = layer.property(PropTag::Zpos) {
        if plane.zpos_min.is_some_and(|min| z < min) {
            return false;
        }
        if plane.zpos_max.is_some_and(|max| z > max) {
            return false;
        }
    }

    if plane.plane_type == PlaneType::Cursor {
        if plane.cursor_max_w > 0 && layer.width() > plane.cursor_max_w {
            return false;
        }
        if plane.cursor_max_h > 0 && layer.height() > plane.cursor_max_h {
            return false;
        }
    }

    true
}

/// Whether two destination rectangles overlap.
///
/// Computed in `i64` so a rectangle placed near `i32::MAX` cannot wrap into a
/// false negative.
#[must_use]
pub fn rects_intersect(a: Rect, b: Rect) -> bool {
    let (ax, ay) = (i64::from(a.x), i64::from(a.y));
    let (bx, by) = (i64::from(b.x), i64::from(b.y));
    ax + i64::from(a.w) > bx
        && bx + i64::from(b.w) > ax
        && ay + i64::from(a.h) > by
        && by + i64::from(b.h) > ay
}

/// Whether two layers overlap on the CRTC.
#[must_use]
pub fn layers_intersect(a: &Layer, b: &Layer) -> bool {
    rects_intersect(a.crtc_rect(), b.crtc_rect())
}

/// Union-find root with path halving.
///
/// Every index passed in comes from `0..parent.len()`, so the indexing cannot
/// go out of range.
fn union_find_root(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]]; // path halving
        x = parent[x];
    }
    x
}

/// Partition layer indices into spatially independent groups.
///
/// Layers that do not overlap cannot contend for the same stacking slot, so
/// each group can be solved on its own — a much smaller search than the whole
/// scene at once.
///
/// # Deviation: deterministic group order
///
/// The C++ collects groups out of an `unordered_map` keyed by union-find root,
/// so their order is unspecified. That matters because the caller consumes a
/// **shared** plane pool: whichever group is processed first takes the
/// contested planes. Groups here come back ordered by their lowest member
/// index, which is stable, reproducible, and follows the caller's own layer
/// order.
///
/// Note this only makes the order *deterministic*, not *prioritized*: cross-
/// group order still does not consult [`keep_priority`]. Raised upstream as
/// drm-cxx#236.
#[must_use]
pub fn split_independent_groups(layers: &[&Layer]) -> Vec<Vec<usize>> {
    if layers.is_empty() {
        return Vec::new();
    }
    if layers.len() == 1 {
        return vec![vec![0]];
    }

    let mut parent: Vec<usize> = (0..layers.len()).collect();

    for i in 0..layers.len() {
        for j in (i + 1)..layers.len() {
            if layers_intersect(layers[i], layers[j]) {
                let (a, b) = (
                    union_find_root(&mut parent, i),
                    union_find_root(&mut parent, j),
                );
                if a != b {
                    parent[a] = b;
                }
            }
        }
    }

    // Bucket by root, then order groups by their lowest member index so the
    // result is deterministic and follows the caller's layer order.
    let mut roots: Vec<(usize, usize)> = (0..layers.len())
        .map(|i| (union_find_root(&mut parent, i), i))
        .collect();
    roots.sort_unstable();

    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current_root = None;
    for (root, index) in roots {
        if Some(root) == current_root
            && let Some(group) = groups.last_mut()
        {
            group.push(index);
        } else {
            current_root = Some(root);
            groups.push(vec![index]);
        }
    }

    groups.sort_by_key(|group| group[0]);
    groups
}
