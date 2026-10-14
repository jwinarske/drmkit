// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! GBM buffer allocation.
//!
//! A thin layer over libgbm, in the shape the rest of drmkit expects: errors
//! as a typed enum, formats as drmkit `FourCC`s, and a buffer that hands out
//! the descriptor a scanout path needs.
//!
//! GBM is how a GPU-rendered surface becomes something KMS can scan out. Where
//! `drmkit-dumb` allocates plain linear memory the CPU writes,
//! this asks the driver for memory its renderer can target — which on real
//! hardware means tiled or compressed layouts, and a modifier saying which.
//!
//! On a driver with no render engine the `drm` backend falls back to dumb
//! allocation, so this still works against vkms: linear, one plane, and a
//! dma-buf that exports. That is what makes it testable without a GPU.
//!
//! # libgbm is opened, not linked
//!
//! The library is loaded on the first [`GbmDevice`], so building needs no
//! libgbm at all and a machine without one gets [`GbmError::NotGbmCapable`]
//! rather than a binary that will not start. Which modifier entry points it
//! has is read there too, so one build runs against Mesa and against a
//! vendor libgbm that predates `gbm_bo_create_with_modifiers2`.
//!
//! [`GbmDevice::from_fd`] takes any DRM descriptor. With the default
//! `drmkit-core` feature off, that is the only way in, and this crate needs
//! nothing of drmkit's.

mod buffer;
mod device;
mod ffi;
mod surface;

#[cfg(test)]
mod tests;

pub use buffer::{GbmBuffer, MapAccess};
pub use device::GbmDevice;
pub use ffi::HIDE_ENV;
pub use surface::GbmSurface;

/// What went wrong allocating or exporting.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GbmError {
    /// The device could not be opened as a GBM device.
    ///
    /// Either libgbm could not be loaded -- not installed, or missing an
    /// entry point this crate cannot do without -- or it would not open the
    /// descriptor, which every DRM node that can allocate anything allows, so
    /// in practice that means it is not a DRM node at all. The string says
    /// which.
    #[error("not a GBM-capable device: {0}")]
    NotGbmCapable(String),

    /// The driver refused the allocation.
    ///
    /// Usually the format, the modifier set, or the usage flags: a driver
    /// advertises what it can scan out through `IN_FORMATS`, and asking for
    /// anything else fails here rather than at commit time. Also returned when
    /// a buffer cannot be exported as a dma-buf.
    #[error("gbm allocation failed: {0}")]
    Allocation(String),

    /// The DRM descriptor could not be duplicated.
    #[error("gbm operation failed: {0}")]
    Io(#[from] rustix::io::Errno),
}
