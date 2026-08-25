// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Cursor themes, and the cursors in them.
//!
//! [`Theme`] indexes the `XCursor` themes installed on a machine and resolves a
//! cursor name -- following each theme's `Inherits=` chain and the alias table
//! that maps modern names onto whatever legacy name a theme happens to ship --
//! to a file on disk. It reads no pixels, which keeps it cheap and testable
//! without a display.
//!
//! [`Cursor`] is the other half: it reads one of those files into frames,
//! hotspots and timing. A cursor file is untrusted input, so the parser bounds
//! every length against the bytes actually present before allocating anything
//! -- see the `xcursor` module for why that is not delegated to a crate.

mod alias;
mod cursor;
mod plane;
mod size;
mod sprite;
mod stage;
mod theme;
mod xcursor;

#[cfg(test)]
mod tests;

pub use cursor::{Cursor, Frame};
pub use plane::{PlanePath, SelectedPlane, select_plane};
pub use size::{preferred_size, probe_size};
pub use sprite::{Placement, Rotation, compose};
pub use stage::{PlaneState, hardware_rotation, plane_origin, stage};
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

    /// The file is not a cursor, is truncated, or contradicts itself.
    #[error("malformed cursor file")]
    Malformed,

    /// The file declares more frames, or larger frames, than are accepted.
    ///
    /// Separate from [`CursorError::Malformed`] because such a file may be
    /// perfectly well-formed -- it is refused for being unreasonable, not for
    /// being broken, and that is worth saying differently.
    #[error("cursor file exceeds the loader's limits")]
    TooLarge,

    /// The file parsed but holds no images.
    #[error("cursor file contains no images")]
    NoImages,

    /// Caller-supplied pixels do not describe an image.
    #[error("not a usable cursor image")]
    InvalidImage,

    /// The file could not be read.
    #[error("io: {0}")]
    Io(String),

    /// No plane on this CRTC can carry a cursor, and legacy was not allowed.
    #[error("no plane on this CRTC can carry a cursor")]
    NoPlane,

    /// The plane is missing a property every plane is required to have.
    ///
    /// A broken driver rather than anything a caller did.
    #[error("the plane lacks a required property")]
    MissingProperty,

    /// A caller-named plane does not exist, cannot feed this CRTC, or cannot
    /// take `ARGB8888`.
    ///
    /// Distinct from [`CursorError::NoPlane`]: the caller overrode the policy
    /// and the override does not work. Falling back silently would leave them
    /// on a plane they did not ask for.
    #[error("the requested plane cannot carry a cursor on this CRTC")]
    PlaneUnusable,
}
