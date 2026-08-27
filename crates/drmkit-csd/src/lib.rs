// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Client-side window decorations.
//!
//! Port of `src/csd/`. Title bars, shadows, rounded corners and the buttons
//! on them — drawn by the client rather than by a compositor, which is what
//! makes them *client-side*.
//!
//! Everything here is CPU work over a pixel buffer. Nothing in this crate
//! touches a DRM device: a decoration is pixels, and where those pixels go is
//! the presenter's business.

mod animator;
mod color;
mod damage;
mod geometry;
mod load;
mod presenter;
mod presenter_fb;
mod presenter_plane;
mod renderer;
mod reservation;
mod shadow;
mod state;
mod theme;

pub use animator::{WindowAnim, ease_out_cubic};
pub use color::{Color, ColorError};
pub use damage::{DamageRect, DamageSlot, compute_damage, intersect_rect, union_rect};
pub use geometry::{DecorationGeometry, decoration_geometry};
pub use load::{ThemeError, load_theme_file, load_theme_str};
pub use presenter::{PropertyWrite, SurfaceRef, Tier, choose_presenter_tier};
pub use presenter_fb::{BlitItem, FbTarget, compose_into_framebuffer, fb_fourcc_for};
pub use presenter_plane::{PlaneError, PlaneSlot, compute_writes};
pub use renderer::{Canvas, DrawError, Renderer, RendererConfig};
pub use reservation::{OverlayReservation, ReserveError};
pub use shadow::{DEFAULT_CAPACITY, Elevation, ShadowCache, ShadowDest, ShadowKey, theme_id};
pub use state::{Dirty, HoverButton, PROGRESS_UNSET, WindowState};
pub use theme::{
    Button, Buttons, Colors, Theme, TitleBar, glass_default, glass_lite, glass_minimal,
};

#[cfg(test)]
mod tests;
