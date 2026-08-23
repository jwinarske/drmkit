// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Tier-1 buffer sources for drmkit scenes.
//!
//! Port of the tier-1 sources from `src/scene/` — the ones that need no GPU,
//! no EGL, and no external media stack. Everything here implements
//! [`LayerBufferSource`](drmkit_scene::LayerBufferSource), so a scene consumes
//! them without knowing which it has.

mod dumb;

pub use dumb::DumbBufferSource;

#[cfg(test)]
mod tests;
