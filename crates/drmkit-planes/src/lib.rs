// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Display-plane capabilities, the layer model, and the plane allocator.
//!
//! Port of `src/planes/` from drm-cxx @ `dc2915b`.
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

mod layer;
mod matching;
mod prop;
mod registry;

pub use layer::{ContentType, Layer, PropertySnapshot, Rect};
pub use matching::BipartiteMatching;
pub use prop::{NUM_PROPS, PropClass, PropTag};
pub use registry::{
    BlendModeValues, ColorEncoding, ColorEncodingValues, ColorPipeline, ColorRange,
    ColorRangeValues, PlaneCapabilities, PlaneRegistry, PlaneType,
};

#[cfg(test)]
mod tests;
