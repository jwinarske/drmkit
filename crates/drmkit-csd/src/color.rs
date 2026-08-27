// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Colours, and reading them from a theme file.

/// An 8-bit-per-channel colour with alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha. Zero is fully transparent.
    pub a: u8,
}

/// Why a colour could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ColorError {
    /// The text was not `#RRGGBB` or `#RRGGBBAA`.
    #[error("expected #RRGGBB or #RRGGBBAA, got {got:?}")]
    Malformed {
        /// What was given.
        got: String,
    },
}

impl Color {
    /// A colour from its channels.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Parse `#RRGGBB` or `#RRGGBBAA`.
    ///
    /// Six digits means fully opaque. That is the CSS convention and the one a
    /// theme author will expect; defaulting to transparent instead would make
    /// every colour written the short way invisible.
    ///
    /// Case-insensitive, because a hand-written theme file mixes both and
    /// refusing lowercase would be a rule with nothing behind it.
    ///
    /// # Errors
    ///
    /// [`ColorError::Malformed`] for anything else — a missing `#`, a wrong
    /// length, or a non-hex digit.
    pub fn from_hex(hex: &str) -> Result<Self, ColorError> {
        let malformed = || ColorError::Malformed {
            got: hex.to_owned(),
        };
        let digits = hex.strip_prefix('#').ok_or_else(malformed)?;
        if !matches!(digits.len(), 6 | 8) {
            return Err(malformed());
        }
        let byte = |index: usize| {
            u8::from_str_radix(digits.get(index..index + 2).ok_or_else(malformed)?, 16)
                .map_err(|_| malformed())
        };
        Ok(Self {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            // Six digits is opaque; eight carries its own alpha.
            a: if digits.len() == 8 { byte(6)? } else { 0xFF },
        })
    }

    /// Pack as `0xAARRGGBB`.
    ///
    /// The layout a 32-bit `ARGB8888` framebuffer word takes on a
    /// little-endian host, which is what the renderer writes into.
    #[must_use]
    pub const fn packed_argb(self) -> u32 {
        ((self.a as u32) << 24) | ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }
}
