// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! DRM dumb buffers: GPU-free, CPU-mappable scanout surfaces.
//!
//! Port of `src/dumb/` plus `src/buffer_mapping.hpp` from drm-cxx @ `dc2915b`.
//!
//! Dumb buffers are KMS's allocation path that needs no accelerator: any driver
//! implementing `DRM_CAP_DUMB_BUFFER` — every modern KMS driver — can hand back
//! a linear, CPU-mappable pixel buffer. They are the right tool for cursors,
//! client-side decorations, software-rendered UI, and test harnesses.
//!
//! # Invariant 6
//!
//! [`Buffer::map`] is a zero-cost view of a mapping established once and held
//! for the buffer's whole life. [`Mapping`] has no [`Drop`] implementation, so
//! "the guard's destructor is a no-op" is structural rather than documented,
//! and the borrow makes outliving the buffer a compile error.
//!
//! This is load-bearing beyond convenience: `DumbScanoutSink`'s pacing depends
//! on `map()` costing nothing per frame.
//!
//! # Example
//!
//! ```no_run
//! use drmkit_core::Device;
//! use drmkit_dumb::{Buffer, Config, MapAccess};
//! use drmkit_fmt::fourcc;
//!
//! let device = Device::open("/dev/dri/card0")?;
//! let mut buffer = Buffer::create(
//!     &device,
//!     &Config { width: 64, height: 64, fourcc: fourcc::XRGB8888, ..Config::default() },
//! )?;
//!
//! // Free to acquire, free to drop -- once per frame is fine.
//! let mut view = buffer.map(MapAccess::Write);
//! if let Some(row) = view.row_mut(0) {
//!     row.fill(0xff);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod buffer;
mod error;
mod mapping;

pub use buffer::{Buffer, Config};
pub use error::{DumbError, Result};
pub use mapping::{MapAccess, Mapping};

#[cfg(test)]
mod tests;
