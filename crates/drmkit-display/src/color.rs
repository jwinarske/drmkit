// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Colour primaries, independent of where they came from.
//!
//! These describe a set of primaries and a white point. An EDID is one source
//! of them and HDR source metadata is another, so they live apart from the
//! parser -- a caller describing the content it is emitting should not have to
//! build the crate against `libdisplay-info` to name a primary.

/// A CIE 1931 chromaticity coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Chromaticity {
    /// x coordinate.
    pub x: f32,
    /// y coordinate.
    pub y: f32,
}

/// The display's colour primaries and white point.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ColorimetryInfo {
    /// Red primary.
    pub red: Chromaticity,
    /// Green primary.
    pub green: Chromaticity,
    /// Blue primary.
    pub blue: Chromaticity,
    /// Default white point.
    pub white: Chromaticity,
    /// Whether the primaries are present. When false the three above are
    /// meaningless rather than zero-valued.
    pub has_primaries: bool,
    /// Whether the white point is present.
    pub has_default_white: bool,
}
