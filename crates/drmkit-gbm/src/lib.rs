// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! GBM buffer allocation.
//!
//! A thin wrapper over the `gbm` crate, in the shape the rest of drmkit expects:
//! errors as a typed enum, formats as drmkit `FourCC`s, and a buffer that hands
//! out the descriptor a scanout path needs.
//!
//! GBM is how a GPU-rendered surface becomes something KMS can scan out. Where
//! [`drmkit-dumb`](drmkit_dumb) allocates plain linear memory the CPU writes,
//! this asks the driver for memory its renderer can target — which on real
//! hardware means tiled or compressed layouts, and a modifier saying which.
//!
//! On a driver with no render engine the `drm` backend falls back to dumb
//! allocation, so this still works against vkms: linear, one plane, and a
//! dma-buf that exports. That is what makes it testable without a GPU.

#![allow(clippy::multiple_crate_versions)]

mod buffer;
mod device;

#[cfg(test)]
mod tests;

pub use buffer::{GbmBuffer, MapAccess};
pub use device::GbmDevice;

/// What went wrong allocating or exporting.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GbmError {
    /// The device could not be opened as a GBM device.
    ///
    /// Every DRM node that can allocate anything supports this, so in practice
    /// it means the descriptor is not a DRM node at all.
    #[error("not a GBM-capable device")]
    NotGbmCapable,

    /// The driver refused the allocation.
    ///
    /// Usually the format, the modifier set, or the usage flags: a driver
    /// advertises what it can scan out through `IN_FORMATS`, and asking for
    /// anything else fails here rather than at commit time.
    #[error("gbm allocation failed: {0}")]
    Allocation(String),

    /// An ioctl failed.
    #[error("gbm operation failed: {0}")]
    Io(#[from] rustix::io::Errno),
}
