// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Display-plane capabilities, the layer model, and the plane allocator.
//!
//! Port of `src/planes/` from drm-cxx @ `dc2915b`.
//!
//! Three pieces, bottom-up:
//!
//! - [`PlaneCapabilities`] and [`PlaneRegistry`] — what each plane on a device
//!   can do: which CRTCs it feeds, which formats and modifiers it scans out,
//!   whether it rotates, where its zpos may sit. Built from a device behind
//!   the `device` feature, or from fixtures a caller supplies, which is what
//!   lets the allocator be tested against hardware nobody has.
//! - [`Layer`] — one thing to put on screen, as a bag of KMS properties plus
//!   the hints that inform placement.
//! - [`Allocator`] — which layer goes on which plane.
//!
//! # Why placement is a search
//!
//! Nothing in the kernel's uAPI answers "can these layers be scanned out
//! together". `IN_FORMATS` prunes, `possible_crtcs` prunes, and neither
//! settles it: bandwidth limits, scaler counts and per-driver rules decide the
//! rest and are not advertised anywhere. The only oracle is an atomic
//! `TEST_ONLY` commit, which costs an ioctl.
//!
//! So the allocator proposes and the kernel disposes, and the whole design is
//! about proposing well enough that few proposals are needed. It seeds with a
//! maximum bipartite matching, orders candidates by a score, backtracks by
//! dropping the layer it can most afford to composite, and spends at most
//! [`Allocator::DEFAULT_MAX_TEST_COMMITS`] test commits on a frame before
//! settling for what it has. Layers that do not overlap are solved
//! independently, so a busy corner of the screen does not make the rest of it
//! expensive.
//!
//! Two caches keep the steady state near zero ioctls: a warm start that
//! re-validates last frame's assignment, and the fast path below.
//!
//! # Invariant 4
//!
//! [`Layer::property_hash`] isolates *content* from *placement*: a frame that
//! changes only content on already-placed layers reuses the cached plane
//! assignment and skips the redundant `TEST_ONLY` commit.
//!
//! The C++ can only document which properties are which. Here the split is
//! [`PropTag::class`], an exhaustive `match`, so a property added without a
//! classification does not compile — the misclassification the invariant warns
//! about cannot reach a running system.

#[cfg(feature = "device")]
mod probe;

mod allocator;
mod layer;
mod matching;
mod output;
mod prop;
mod registry;
mod scoring;

pub use allocator::{
    Allocation, Allocator, AlwaysAccept, Diagnostics, LayerId, LayerRef, PlaneAssignment,
    TestCache, TestCommitter, TestFailure,
};
pub use layer::{ContentType, Layer, PropertySnapshot, Rect};
pub use matching::BipartiteMatching;
pub use output::Output;
pub use prop::{NUM_PROPS, PropClass, PropTag};
pub use registry::{
    BlendModeValues, ColorEncoding, ColorEncodingValues, ColorPipeline, ColorRange,
    ColorRangeValues, PlaneCapabilities, PlaneRegistry, PlaneType,
};
pub use scoring::{
    ScoreContext, WARM_STABILITY_BONUS, bandwidth_class_bonus, cost_bias, keep_priority,
    layer_priority, layers_intersect, plane_statically_compatible, rects_intersect, score_pair,
    split_independent_groups,
};

#[cfg(test)]
mod tests;
