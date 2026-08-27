// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What a decorated window currently is.

/// Which window button the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HoverButton {
    /// None of them.
    #[default]
    None,
    /// Close.
    Close,
    /// Minimize.
    Minimize,
    /// Maximize.
    Maximize,
}

/// What changed since the decoration was last drawn.
///
/// A bitmask rather than a bool: redrawing a title bar because the pointer
/// moved over a button costs a rounded-rect fill; redrawing it because the
/// geometry changed costs a blur. A caller that cannot tell them apart pays
/// the second price for the first event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dirty(u32);

impl Dirty {
    /// Nothing changed.
    pub const NONE: Self = Self(0);
    /// The title text.
    pub const TITLE: Self = Self(1 << 0);
    /// Focus.
    pub const FOCUS: Self = Self(1 << 1);
    /// Which button is hovered.
    pub const HOVER: Self = Self(1 << 2);
    /// The window's size or position.
    pub const GEOMETRY: Self = Self(1 << 3);
    /// An animation advanced.
    pub const ANIMATION: Self = Self(1 << 4);
    /// Everything. What a freshly created state carries, since nothing has
    /// been drawn yet and every part of it is new.
    pub const ALL: Self = Self(u32::MAX);

    /// Whether any of `other`'s bits are set here.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Both sets of bits.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether nothing is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// The progress value a state carries before an animator has written one.
///
/// Out of the `0..=1` range on purpose: a caller can tell "not yet animated"
/// from "animated to zero", which are different things — the first means draw
/// it settled, the second means draw it mid-transition at the start.
pub const PROGRESS_UNSET: f32 = -1.0;

/// A decorated window, as the renderer sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowState {
    /// The title text.
    pub title: String,
    /// Whether the window has focus.
    pub focused: bool,
    /// Which button is hovered.
    pub hover: HoverButton,
    /// What has changed since the last draw.
    pub dirty: Dirty,
    /// How far through the focus transition, or [`PROGRESS_UNSET`].
    pub focus_progress: f32,
    /// How far through the hover transition, or [`PROGRESS_UNSET`].
    pub hover_progress: f32,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            title: String::new(),
            focused: false,
            hover: HoverButton::None,
            dirty: Dirty::ALL,
            focus_progress: PROGRESS_UNSET,
            hover_progress: PROGRESS_UNSET,
        }
    }
}
