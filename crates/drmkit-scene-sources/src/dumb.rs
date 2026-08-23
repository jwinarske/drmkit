// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A single-buffer source backed by a dumb buffer.
//!
//! Port of `src/scene/dumb_buffer_source.{hpp,cpp}`.

use drmkit_core::Device;
use drmkit_dumb::{Buffer, Config, MapAccess, Mapping};
use drmkit_scene::{
    AcquiredBuffer, BindingModel, DamageRect, LayerBufferSource, SourceError, SourceFormat,
};

/// The simplest source: one CPU-writable linear buffer, created once and reused
/// every frame.
///
/// `acquire` returns the cached framebuffer id; `release` does nothing, because
/// there is nowhere to hand the buffer back to. Pixel access goes through
/// [`map`](LayerBufferSource::map), backed by the dumb buffer's pinned
/// kernel-coherent mapping — so acquiring a view costs nothing per frame
/// (invariant 6).
///
/// Suitable for software-rendered cursors and decorations, test patterns, and
/// any layer whose content lives in CPU memory and does not need double
/// buffering. **Not** suitable where a producer is racing scanout: this is a
/// single buffer, so a write can tear against the display engine reading it.
/// That is acceptable at the rate a decoration frame is redrawn and not
/// otherwise; a ring source is the answer there.
#[derive(Debug)]
pub struct DumbBufferSource {
    buffer: Buffer,
    format: SourceFormat,
    /// Reported on the next acquire, then cleared.
    pending_damage: Vec<DamageRect>,
}

impl DumbBufferSource {
    /// Allocate a single buffer of the given size and format.
    ///
    /// The kernel zero-fills a dumb buffer at creation, so the first acquire
    /// presents a fully transparent image rather than whatever was in memory.
    ///
    /// # Errors
    ///
    /// Whatever the allocation failed with.
    pub fn create(
        device: &Device,
        width: u32,
        height: u32,
        fourcc: u32,
    ) -> Result<Self, drmkit_dumb::DumbError> {
        let buffer = Buffer::create(
            device,
            &Config {
                width,
                height,
                fourcc,
                ..Config::default()
            },
        )?;

        Ok(Self {
            format: SourceFormat {
                fourcc,
                // A dumb buffer is linear by construction.
                modifier: 0,
                width,
                height,
            },
            buffer,
            pending_damage: Vec::new(),
        })
    }

    /// Report the dirty region for the **next** acquire, then cleared.
    ///
    /// Empty means full-frame. A software source that repaints only part of its
    /// buffer sets this so the scene can emit `FB_DAMAGE_CLIPS` and the driver
    /// repaints only what changed — a bandwidth win, and what lets a
    /// self-refresh panel stay in self-refresh.
    ///
    /// Replaces any prior unconsumed set rather than accumulating: the caller
    /// knows what it just painted, and merging stale regions would over-report.
    pub fn set_damage(&mut self, rects: &[DamageRect]) {
        self.pending_damage.clear();
        self.pending_damage.extend_from_slice(rects);
    }

    /// The underlying buffer, for a caller that needs its framebuffer id
    /// directly — the legacy cursor path, for instance.
    #[must_use]
    pub const fn buffer(&self) -> &Buffer {
        &self.buffer
    }
}

impl LayerBufferSource for DumbBufferSource {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        let fb_id = self
            .buffer
            .fb_id()
            .ok_or(SourceError::Failed(rustix::io::Errno::INVAL))?;

        Ok(AcquiredBuffer {
            fb_id,
            token: 0,
            acquire_fence: None,
            // Handed over, not copied: the damage describes one frame, and the
            // next acquire starts from full-frame unless told otherwise.
            damage: std::mem::take(&mut self.pending_damage),
        })
    }

    fn release(&mut self, _acquired: AcquiredBuffer) {
        // Single-buffer source: this object owns the buffer permanently and
        // hands the same one out next acquire. There is nothing to return.
    }

    fn binding_model(&self) -> BindingModel {
        BindingModel::SceneSubmitsFbId
    }

    fn format(&self) -> SourceFormat {
        self.format
    }

    fn map(&mut self, access: MapAccess) -> Result<Mapping<'_>, SourceError> {
        Ok(self.buffer.map(access))
    }

    fn on_session_paused(&mut self) {
        // Nothing outstanding. Acquire is driven by the scene's commit path,
        // which does not run while paused, so the state is safe as it stands
        // until `on_session_resumed` re-allocates against a live descriptor.
    }

    fn on_session_resumed(&mut self, device: &Device) -> Result<(), SourceError> {
        // The old descriptor is dead, so `forget` drops the GEM handle and
        // framebuffer id without issuing ioctls against it -- the kernel
        // reclaims them when the descriptor closes -- and unmaps the CPU
        // mapping, which is descriptor-independent.
        //
        // Snapshot the shape first: it is the source of truth once the buffer
        // has been forgotten.
        let SourceFormat {
            fourcc,
            width,
            height,
            ..
        } = self.format;

        self.buffer.forget();

        self.buffer = Buffer::create(
            device,
            &Config {
                width,
                height,
                fourcc,
                ..Config::default()
            },
        )
        .map_err(|_| SourceError::Failed(rustix::io::Errno::NOMEM))?;

        // `format` is re-affirmed, not reassigned: dimensions and modifier are
        // unchanged, and consumers rely on `format()` returning the same value
        // across a resume.
        Ok(())
    }
}
