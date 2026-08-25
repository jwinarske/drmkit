// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Cursor themes, and the cursors in them.
//!
//! [`Theme`] indexes the `XCursor` themes installed on a machine and resolves a
//! cursor name -- following each theme's `Inherits=` chain and the alias table
//! that maps modern names onto whatever legacy name a theme happens to ship --
//! to a file on disk. It reads no pixels, which keeps it cheap and testable
//! without a display.

mod alias;
mod theme;

#[cfg(test)]
mod tests;

pub use theme::{Theme, ThemeResolution, default_search_paths};

/// What can go wrong finding a cursor.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CursorError {
    /// The cursor name is empty, `.`, `..`, or contains a path separator.
    ///
    /// A cursor name is an identifier, not a path. Refusing the ambiguous
    /// cases keeps a caller-supplied name from composing into somewhere else
    /// on the filesystem.
    #[error("not a usable cursor name")]
    InvalidName,

    /// No cursor theme directory was readable at all.
    ///
    /// Distinct from [`CursorError::NotFound`]: this one means nothing is
    /// installed, which is a different thing to say to a user.
    #[error("no cursor themes are installed")]
    NoThemes,

    /// No theme on the search path has this cursor.
    #[error("no theme provides that cursor")]
    NotFound,
}
