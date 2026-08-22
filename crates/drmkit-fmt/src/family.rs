// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Parameterized modifier families.
//!
//! Most modifiers match a plane's `IN_FORMATS` entry by equality. A few vendor
//! families are *parameterized*: the plane advertises a base modifier, and a
//! producer bakes a buffer-specific parameter into the value it hands over. The
//! two are not equal, but the buffer is still scannable, because the kernel
//! reads the parameter from the framebuffer rather than from `IN_FORMATS`.
//!
//! This module generalizes the special case introduced upstream in PR #231 for
//! Broadcom SAND (plan §4.7). The C++ side spells that case out inline in
//! `layer_fits_plane`; carrying it as a family keeps the next vendor from
//! becoming a second inline branch.
//!
//! # Behavioral parity
//!
//! Generalizing the *mechanism* must not widen the *behavior*. Today exactly
//! one family declares a non-exact match — Broadcom SAND — and it matches the
//! upstream predicate byte for byte. Arm AFBC is modeled here for description
//! and classification, but its block-size and flag variants match **exactly**,
//! because upstream requires exact matches for them and loosening that would
//! let a plane accept a layout the display engine cannot fetch.
//!
//! Adding a family is a behavior change to plane eligibility. It needs the same
//! evidence PR #231 carried: a driver that demonstrably accepts the parameter
//! range on a plane advertising the base.

use crate::modifier::{Modifier, broadcom, vendor};

/// The family a modifier belongs to, for eligibility matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ModifierFamily {
    /// The modifier matches only itself. Everything not named below.
    Exact,
    /// Broadcom SAND, one of `SAND64` / `SAND128` / `SAND256`.
    ///
    /// The low byte is the layout code; bits 8..56 carry the column height. A
    /// plane advertises the base (column height 0); a producer bakes the coded
    /// image height in. `rpi-hevc-dec` exporting `SAND128` with the column
    /// height set is the case PR #231 fixed.
    ///
    /// `SAND32` (code 2) is deliberately **not** a member: the upstream commit
    /// names `SAND64/128/256` explicitly, and widening the range here would be
    /// a behavior change with no driver evidence behind it.
    BroadcomSand {
        /// The SAND layout code: 3, 4, or 5.
        code: u64,
    },
}

impl ModifierFamily {
    /// The base modifier a plane advertises for this family, if the family is
    /// parameterized.
    ///
    /// [`ModifierFamily::Exact`] has no base — such a modifier is only ever
    /// eligible against itself.
    #[must_use]
    pub const fn base(self) -> Option<Modifier> {
        match self {
            Self::Exact => None,
            Self::BroadcomSand { code } => {
                Some(Modifier(crate::modifier::mod_code(vendor::BROADCOM, code)))
            }
        }
    }
}

impl Modifier {
    /// Which family this modifier belongs to.
    ///
    /// ```
    /// use drmkit_fmt::{Modifier, ModifierFamily, sand128_col_height};
    ///
    /// // A producer's SAND128 with a baked-in column height.
    /// let produced = sand128_col_height(1080);
    /// assert!(matches!(produced.family(), ModifierFamily::BroadcomSand { code: 4 }));
    ///
    /// // The base a plane advertises.
    /// assert_eq!(produced.family().base(), Some(sand128_col_height(0)));
    ///
    /// // Everything else matches only itself.
    /// assert_eq!(Modifier::LINEAR.family(), ModifierFamily::Exact);
    /// assert_eq!(Modifier::LINEAR.family().base(), None);
    /// ```
    #[must_use]
    pub const fn family(self) -> ModifierFamily {
        if self.vendor() == vendor::BROADCOM {
            let code = self.0 & 0xff;
            // SAND64 / SAND128 / SAND256, matching the upstream predicate.
            if code >= broadcom::SAND64 && code <= broadcom::SAND256 {
                return ModifierFamily::BroadcomSand { code };
            }
        }
        ModifierFamily::Exact
    }
}

/// Build a Broadcom `SAND128` modifier with an explicit column height.
///
/// Column height 0 is the base a plane advertises in `IN_FORMATS`.
#[must_use]
pub const fn sand128_col_height(height: u64) -> Modifier {
    Modifier(crate::modifier::mod_code(
        vendor::BROADCOM,
        (height << 8) | broadcom::SAND128,
    ))
}

/// The column height baked into a SAND modifier, or `None` if it is not SAND.
#[must_use]
pub const fn sand_col_height(m: Modifier) -> Option<u64> {
    match m.family() {
        ModifierFamily::BroadcomSand { .. } => Some((m.0 >> 8) & ((1 << 48) - 1)),
        ModifierFamily::Exact => None,
    }
}
