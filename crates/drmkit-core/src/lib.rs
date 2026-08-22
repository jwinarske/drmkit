// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Core DRM/KMS device access for drmkit.
//!
//! Port of `src/core/` plus `src/modeset/atomic.{hpp,cpp}` from drm-cxx @
//! `dc2915b`.
//!
//! Three things live here:
//!
//! - [`Device`] — an open DRM node, owning or borrowing its descriptor.
//! - [`PropertyStore`] — a per-object property cache, so building a frame's
//!   atomic request is hash-map lookups rather than an ioctl per property.
//! - [`AtomicRequest`] — accumulated property writes, testable and committable.
//!
//! # Relationship to the `drm` crate
//!
//! Per DR-2 drmkit **wraps** the Smithay `drm` crate and never re-binds the
//! ioctls. [`Device`] implements [`drm::Device`] and [`drm::control::Device`],
//! so the whole KMS surface those traits expose is available directly. What
//! this crate adds is the parts the C++ gives a different contract: descriptor
//! ownership, the property cache, and the atomic request's export seam.
//!
//! # Example
//!
//! ```no_run
//! use drmkit_core::{AtomicRequest, Device, ObjectType, PropertyStore};
//! use drm::control::AtomicCommitFlags;
//!
//! let device = Device::open("/dev/dri/card0")?;
//! device.enable_universal_planes()?;
//! device.enable_atomic()?;
//!
//! let mut props = PropertyStore::new();
//! props.cache_properties(&device, 31, ObjectType::Plane)?;
//!
//! let mut request = AtomicRequest::new();
//! request.add_property(31, props.property_id(31, "FB_ID")?, 42)?;
//!
//! request.test(&device, AtomicCommitFlags::empty())?;
//! # Ok::<(), drmkit_core::CoreError>(())
//! ```

mod atomic;
mod device;
mod error;
mod property;

pub use atomic::{AtomicRequest, PropertyWrite};
pub use device::Device;
pub use error::{CoreError, Result};
pub use property::{ObjectType, PropertyInfo, PropertyStore};

/// Re-exported so consumers need not depend on `drm` directly for the types
/// that appear in this crate's signatures.
pub use drm::ClientCapability;
pub use drm::control::AtomicCommitFlags;

#[cfg(test)]
mod tests;
