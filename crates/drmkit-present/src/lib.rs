// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The present spine: getting rendered frames onto a display, repeatedly.
//!
//! Port of `src/present/` from drm-cxx @ `4a0b64a`, less the GPU producers.
//!
//! Where [`drmkit_scene`](https://docs.rs/drmkit-scene) answers "what goes on
//! which plane this frame", this crate answers the questions around it: which
//! buffer to draw into, how much of it needs redrawing, and whether the frame
//! is worth submitting at all.
//!
//! # Why it is a separate crate
//!
//! A producer that renders on the GPU needs EGL or Vulkan; one that writes
//! pixels with the CPU needs neither. Keeping the spine core-only means a
//! GPU-less build — a kiosk, a test rig, an instrument panel — links no
//! graphics stack at all. The producers that do live in their own crates and
//! load their libraries at runtime.
//!
//! # Not here yet
//!
//! Being built up. [`BufferRing`], [`negotiate`] and [`FrameEconomy`] are in;
//! `ScanoutBackend`, `DumbScanoutSink` and `DumbRingSource` follow, and the
//! GBM, GL and Vulkan producers are separate crates after that.

mod buffer_ring;
#[cfg(feature = "device")]
mod dumb_ring;
mod frame_economy;
mod negotiate;

pub use buffer_ring::{BufferRing, Lease, Rect, Repaint};
#[cfg(feature = "device")]
pub use dumb_ring::{DumbRingSource, PaintError};
pub use frame_economy::{FrameAction, FrameEconomy};
pub use negotiate::{negotiate, negotiate_for_format};

#[cfg(test)]
mod tests;
