// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The closed set of KMS plane properties a [`Layer`](crate::Layer) carries.

/// Property tags a layer recognizes.
///
/// The set is **closed**: every KMS plane property the scene and allocator
/// emit lives here, and an unrecognized name handed to
/// [`Layer::set_property_by_name`](crate::Layer::set_property_by_name) is
/// dropped — a kernel commit would reject it with `EINVAL` anyway, so
/// accepting it would only hide the typo.
///
/// Adding a property means adding a variant here and extending
/// [`PropTag::name`] and [`PropTag::class`]. Both are exhaustive `match`es, so
/// the compiler names every place that needs updating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum PropTag {
    /// `FB_ID` — the framebuffer to scan out.
    FbId,
    /// `FB_MODIFIER` — the layout of that framebuffer.
    FbModifier,
    /// `CRTC_ID` — which CRTC the plane feeds.
    CrtcId,
    /// `CRTC_X` — destination x, in pixels.
    CrtcX,
    /// `CRTC_Y` — destination y, in pixels.
    CrtcY,
    /// `CRTC_W` — destination width, in pixels.
    CrtcW,
    /// `CRTC_H` — destination height, in pixels.
    CrtcH,
    /// `SRC_X` — source x, 16.16 fixed point.
    SrcX,
    /// `SRC_Y` — source y, 16.16 fixed point.
    SrcY,
    /// `SRC_W` — source width, 16.16 fixed point.
    SrcW,
    /// `SRC_H` — source height, 16.16 fixed point.
    SrcH,
    /// `rotation` — rotate/reflect bitmask.
    Rotation,
    /// `alpha` — per-plane alpha, u16.
    Alpha,
    /// `zpos` — stacking order.
    Zpos,
    /// `pixel_format` — the `FourCC`.
    PixelFormat,
    /// `IN_FENCE_FD` — a fence the flip waits on.
    InFenceFd,
}

/// What a property describes, for the FB-only fast path.
///
/// This is the **corollary of invariant 4** the C++ can only state in prose:
///
/// > `property_hash` isolates content from placement — every placement or
/// > format property moves the hash; `FB_ID` and `IN_FENCE_FD` do not. A
/// > property misclassified as content would let a real change skip the test.
///
/// Here it is data, consumed by [`PropTag::class`]'s exhaustive `match`. A new
/// variant added without a classification does not compile, so the failure the
/// C++ warns about — a property silently treated as content — cannot reach a
/// running system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PropClass {
    /// Changes every frame and does not affect plane compatibility.
    ///
    /// Excluded from [`Layer::property_hash`](crate::Layer::property_hash), so
    /// a frame that changes only these can reuse the cached plane assignment
    /// and skip the redundant `TEST_ONLY` commit.
    Content,
    /// Describes where or how the plane scans out.
    ///
    /// Any change here must move the hash, forcing a re-test. Getting this
    /// wrong in the permissive direction — calling a placement property
    /// content — is what would let a real geometry change skip validation.
    Placement,
}

/// Number of distinct property tags.
pub const NUM_PROPS: usize = PropTag::ALL.len();

impl PropTag {
    /// Every tag, in enum order.
    ///
    /// Listing them explicitly rather than deriving a count keeps the
    /// exhaustiveness of [`PropTag::class`] meaningful: a variant added
    /// without being added here fails the completeness test below it.
    pub const ALL: [Self; 16] = [
        Self::FbId,
        Self::FbModifier,
        Self::CrtcId,
        Self::CrtcX,
        Self::CrtcY,
        Self::CrtcW,
        Self::CrtcH,
        Self::SrcX,
        Self::SrcY,
        Self::SrcW,
        Self::SrcH,
        Self::Rotation,
        Self::Alpha,
        Self::Zpos,
        Self::PixelFormat,
        Self::InFenceFd,
    ];

    /// Index into a layer's property array.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The canonical KMS property name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::FbId => "FB_ID",
            Self::FbModifier => "FB_MODIFIER",
            Self::CrtcId => "CRTC_ID",
            Self::CrtcX => "CRTC_X",
            Self::CrtcY => "CRTC_Y",
            Self::CrtcW => "CRTC_W",
            Self::CrtcH => "CRTC_H",
            Self::SrcX => "SRC_X",
            Self::SrcY => "SRC_Y",
            Self::SrcW => "SRC_W",
            Self::SrcH => "SRC_H",
            Self::Rotation => "rotation",
            Self::Alpha => "alpha",
            Self::Zpos => "zpos",
            Self::PixelFormat => "pixel_format",
            Self::InFenceFd => "IN_FENCE_FD",
        }
    }

    /// Whether this property describes content or placement.
    ///
    /// See [`PropClass`]. The `match` is exhaustive by construction: adding a
    /// variant without classifying it is a compile error, which is the whole
    /// reason this is a function rather than a lookup table with a default.
    #[must_use]
    pub const fn class(self) -> PropClass {
        match self {
            // Changes every frame: a new buffer, a new fence descriptor.
            // Neither says anything about whether the plane can scan it out.
            Self::FbId | Self::InFenceFd => PropClass::Content,

            // Everything else describes where or how the plane scans out, so
            // a change must invalidate the cached placement. `FB_MODIFIER`
            // and `PixelFormat` belong here because a layout or format change
            // can make a previously valid plane invalid.
            Self::FbModifier
            | Self::CrtcId
            | Self::CrtcX
            | Self::CrtcY
            | Self::CrtcW
            | Self::CrtcH
            | Self::SrcX
            | Self::SrcY
            | Self::SrcW
            | Self::SrcH
            | Self::Rotation
            | Self::Alpha
            | Self::Zpos
            | Self::PixelFormat => PropClass::Placement,
        }
    }

    /// Look up a tag by its canonical KMS name.
    ///
    /// A linear scan over 16 entries: branchless, cache-friendly, and faster
    /// than a hash map at this size — the same reasoning the C++ records.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tag| tag.name() == name)
    }
}
