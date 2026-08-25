// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Per-CRTC cache of the `HDR_OUTPUT_METADATA` blob.
//!
//! Holds the blob most recently handed to each CRTC and dedups by content
//! hash, so metadata that has not changed does not rebuild a kernel blob every
//! frame.
//!
//! Replacing a blob does not destroy the old one straight away. Between
//! creating the replacement and the atomic commit that repoints the connector
//! property, the kernel is still holding the old id -- so the outgoing blob
//! goes on a queue and [`HdrMetadataCache::acknowledge_committed`] drains it
//! once the commit has landed. Draining early frees a blob the display is
//! still using.

use std::collections::HashMap;

use drmkit_core::{Device, Result as CoreResult};

use crate::hdr::{HdrBlob, HdrMetadataBlob, HdrSourceMetadata, hdr_metadata_hash};

/// A cached blob and the hash of what produced it.
#[derive(Debug)]
struct Entry<B> {
    blob: B,
    hash: u64,
}

/// Per-CRTC `HDR_OUTPUT_METADATA` blobs, deduped by content.
///
/// Generic over the blob so the caching logic can be tested without a DRM
/// device -- see [`HdrBlob`].
#[derive(Debug)]
pub struct HdrMetadataCache<B> {
    active: HashMap<u32, Entry<B>>,
    pending_destruction: Vec<B>,
}

impl<B> Default for HdrMetadataCache<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B> HdrMetadataCache<B> {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active: HashMap::new(),
            pending_destruction: Vec::new(),
        }
    }

    /// Blobs waiting for a commit to land before they can be destroyed.
    #[must_use]
    pub fn pending_destruction_count(&self) -> usize {
        self.pending_destruction.len()
    }

    /// Destroy everything queued for destruction.
    ///
    /// Call this *after* the atomic commit that references the new blob has
    /// landed. By then the connector property has moved off every older id, so
    /// letting them go is safe.
    pub fn acknowledge_committed(&mut self) {
        self.pending_destruction.clear();
    }
}

impl<B: HdrBlob> HdrMetadataCache<B> {
    /// The blob id currently cached for `crtc_id`, or `0` if there is none.
    #[must_use]
    pub fn current_blob_id(&self, crtc_id: u32) -> u32 {
        self.active
            .get(&crtc_id)
            .map_or(0, |entry| entry.blob.blob_id())
    }

    /// The content hash currently cached for `crtc_id`, or `0` if there is
    /// none.
    ///
    /// A real hash of exactly zero is possible and vanishingly unlikely; use
    /// [`HdrMetadataCache::current_blob_id`] to ask whether anything is
    /// cached.
    #[must_use]
    pub fn current_content_hash(&self, crtc_id: u32) -> u64 {
        self.active.get(&crtc_id).map_or(0, |entry| entry.hash)
    }

    /// Set or clear the metadata for `crtc_id`, returning the blob id to write
    /// to the connector's `HDR_OUTPUT_METADATA` property.
    ///
    /// `None` clears: returns `0` and queues any cached blob. `Some` whose
    /// hash matches what is cached is a no-op that returns the cached id
    /// without touching the kernel. Otherwise `factory` builds a replacement
    /// and the outgoing blob is queued.
    ///
    /// The factory runs only when a new blob is actually needed, so a caller
    /// can put the expensive part inside it.
    ///
    /// # Errors
    ///
    /// Whatever `factory` returns. A failing factory leaves the cache exactly
    /// as it was -- the previous blob stays live and stays cached, because a
    /// caller that ignores the error and commits anyway should show the old
    /// metadata rather than a freed blob or none at all.
    pub fn set_with<E, F>(
        &mut self,
        crtc_id: u32,
        src: Option<&HdrSourceMetadata>,
        factory: F,
    ) -> Result<u32, E>
    where
        F: FnOnce(&HdrSourceMetadata) -> Result<B, E>,
    {
        let Some(src) = src else {
            if let Some(entry) = self.active.remove(&crtc_id) {
                self.pending_destruction.push(entry.blob);
            }
            return Ok(0);
        };

        let hash = hdr_metadata_hash(src);
        if let Some(entry) = self.active.get(&crtc_id)
            && entry.hash == hash
        {
            return Ok(entry.blob.blob_id());
        }

        // Built before the old entry is disturbed, so a failure here is a
        // no-op rather than a cache that has already dropped what it had.
        let blob = factory(src)?;
        let blob_id = blob.blob_id();
        if let Some(previous) = self.active.insert(crtc_id, Entry { blob, hash }) {
            self.pending_destruction.push(previous.blob);
        }
        Ok(blob_id)
    }

    /// Forget every blob without asking the kernel to destroy any of them.
    ///
    /// For session loss -- a libseat pause revokes the descriptor, and the
    /// kernel reclaimed every blob on it at that moment. Destroying them
    /// through the replacement descriptor would at best fail and at worst free
    /// an unrelated blob that was given the same id there.
    pub fn clear_for_session_loss(&mut self) {
        for (_, entry) in self.active.drain() {
            entry.blob.forget();
        }
        for blob in self.pending_destruction.drain(..) {
            blob.forget();
        }
    }

    /// Destroy every blob, cached and queued alike.
    ///
    /// For rebinding to a different CRTC or connector on the *same* descriptor,
    /// where nothing references the old blobs any more and holding them until
    /// the descriptor closes would leak. Unlike
    /// [`HdrMetadataCache::clear_for_session_loss`], this really does destroy
    /// them, so the descriptor has to still be live.
    pub fn flush(&mut self) {
        self.active.clear();
        self.pending_destruction.clear();
    }
}

impl<'a> HdrMetadataCache<HdrMetadataBlob<'a>> {
    /// Set or clear the metadata for `crtc_id`, building blobs on `device`.
    ///
    /// [`HdrMetadataCache::set_with`] with the obvious factory.
    ///
    /// # Errors
    ///
    /// [`drmkit_core::CoreError`] if the kernel refuses to create the blob.
    pub fn set(
        &mut self,
        device: &'a Device,
        crtc_id: u32,
        src: Option<&HdrSourceMetadata>,
    ) -> CoreResult<u32> {
        self.set_with(crtc_id, src, |s| HdrMetadataBlob::create(device, s))
    }
}
