// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Reuse [`ExternalDmaBufSource`]s across frames.
//!
//! A producer that re-presents the same handful of DMA-BUF-backed buffers — an
//! EGL or Vulkan swapchain's images, a V4L2 capture ring — hands the same
//! descriptors back round after round. Importing each one and registering a
//! fresh KMS framebuffer every frame is pure waste: two ioctls per plane per
//! frame to arrive at exactly the framebuffer id the kernel already had.
//!
//! Keyed by a caller-chosen buffer identity, so steady-state per-frame work is
//! the atomic commit and nothing else.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use drmkit_core::Device;
use drmkit_scene::{LayerBufferSource, SourceFormat};

use crate::external::{ExternalDmaBufSource, ExternalError, ExternalPlane};

/// Caches one [`ExternalDmaBufSource`] per buffer identity.
///
/// The key is whatever the producer uses to tell its own buffers apart — a
/// swapchain image index, a V4L2 buffer index, a handle. Upstream types it as
/// `uintptr_t` and expects a cast pointer; a `u64` says the same thing without
/// inviting a raw pointer across the API, and a producer that wants pointer
/// identity can still cast one in.
#[derive(Default)]
pub struct DmaBufSourceCache {
    entries: HashMap<u64, ExternalDmaBufSource>,
}

impl DmaBufSourceCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The source for `key`, importing the buffer on first sight.
    ///
    /// A cached source whose geometry, format or modifier no longer matches is
    /// **replaced**, not returned: the producer has reused the key for a
    /// different buffer, and handing back the old source would scan out the
    /// wrong memory at the wrong stride. Reusing a key that way is normal —
    /// a swapchain index survives a window resize.
    ///
    /// # Errors
    ///
    /// [`ExternalError`] if the import fails. Nothing is cached in that case,
    /// so a later call with a working descriptor still succeeds.
    pub fn get_or_create(
        &mut self,
        key: u64,
        device: &Device,
        format: SourceFormat,
        planes: &[ExternalPlane<'_>],
    ) -> Result<&mut ExternalDmaBufSource, ExternalError> {
        let stale = self
            .entries
            .get(&key)
            .is_some_and(|source| LayerBufferSource::format(source) != format);
        if stale {
            self.entries.remove(&key);
        }

        match self.entries.entry(key) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                // Imported before the insert, so a failure leaves the cache as
                // it was. An entry left behind by a failed import would be
                // handed out on every later frame and the layer would never
                // recover from what may have been a transient error.
                let source = ExternalDmaBufSource::create(device, format, planes, None)?;
                Ok(entry.insert(source))
            }
        }
    }

    /// The source cached under `key`, if any.
    #[must_use]
    pub fn find(&self, key: u64) -> Option<&ExternalDmaBufSource> {
        self.entries.get(&key)
    }

    /// Drop the source under `key`, returning whether one was there.
    ///
    /// For when the producer frees that buffer. The import and its framebuffer
    /// go with it, so a later `get_or_create` on the same key imports afresh.
    pub fn evict(&mut self, key: u64) -> bool {
        self.entries.remove(&key).is_some()
    }

    /// Drop every cached source.
    ///
    /// For session loss or a device rebind, where every framebuffer id in here
    /// belongs to a descriptor that is gone.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// How many sources are cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
