// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Tier-1 buffer sources for drmkit scenes.
//!
//! Port of the tier-1 sources from `src/scene/` — the ones that need no GPU,
//! no EGL, and no external media stack. Everything here implements
//! [`LayerBufferSource`](drmkit_scene::LayerBufferSource), so a scene consumes
//! them without knowing which it has.

mod cache;
mod dumb;
mod external;
mod presenter;

pub use cache::DmaBufSourceCache;
pub use dumb::DumbBufferSource;
pub use external::{ExternalDmaBufSource, ExternalError, ExternalPlane};
pub use presenter::{Acquired, MAX_DAMAGE, Present, RingPresenter, SlotKey};

#[cfg(test)]
mod tests;
