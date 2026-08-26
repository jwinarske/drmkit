// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! How the scene displays a layer's buffer.
//!
//! Port of the geometry core of `src/scene/display_params.hpp`.
//!
//! Separated from [`SourceFormat`](crate::SourceFormat) along the KMS concept
//! boundary: the source describes what the buffer **is**, these describe how the
//! plane scanning it out should be **configured**. The same buffer can be shown
//! several ways, and the same plane configuration can scan different buffers.
//!
//! # Scope
//!
//! The colorimetry, HDR metadata, and per-plane color-pipeline fields the C++
//! carries here need `drmkit-display`, which lands in phase 4. This module has
//! the geometry every commit needs; the color surface follows its dependency.

pub use drmkit_planes::Rect;

/// How a layer should be displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DisplayParams {
    /// Region of the **buffer** to show.
    ///
    /// A zero width or height means "the buffer's full extent" — see
    /// [`lower_layer`](crate::lower_layer), where that resolution happens.
    pub src_rect: Rect,

    /// Region of the **CRTC** to show it on.
    ///
    /// `x`/`y` are signed: a plane may extend off-screen.
    pub dst_rect: Rect,

    /// Rotate and reflect bits, as a `DRM_MODE_ROTATE_*` /
    /// `DRM_MODE_REFLECT_*` mask. Zero means no rotation, and nothing is
    /// written.
    pub rotation: u64,

    /// Per-plane alpha, if the caller set one.
    pub alpha: Option<u16>,

    /// Stacking position, if the caller set one.
    ///
    /// Left unset deliberately by most callers — see
    /// [`lower_layer`](crate::lower_layer) for why writing a default here
    /// rather than leaving it alone is actively harmful.
    pub zpos: Option<u64>,

    /// The container colorimetry this layer's content is in.
    ///
    /// Feeds the scene's choice of connector `Colorspace` -- the widest-gamut
    /// layer wins -- and seeds the mastering primaries when the frame is HDR.
    /// `None` means the producer did not say, and the scene leaves the
    /// connector property alone rather than guessing.
    pub color_primaries: Option<crate::signaling::ColorPrimaries>,

    /// The transfer function the content is encoded with.
    ///
    /// `None` is the SDR transfer. When *any* layer in a frame reports PQ or
    /// HLG the scene declares HDR on the connector.
    pub source_eotf: Option<drmkit_display::TransferFunction>,
}

/// Convert whole pixels to the 16.16 fixed point KMS expects for `SRC_*`.
///
/// The kernel encodes source rectangles this way so a compositor can express
/// subpixel positioning; the destination rectangle stays in whole pixels.
#[must_use]
pub const fn to_16_16(pixels: u32) -> u64 {
    (pixels as u64) << 16
}
