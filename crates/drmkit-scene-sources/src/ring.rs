// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! An N-slot ring over caller-owned DMA-BUFs, for a rotating producer.
//!
//! Where [`ExternalDmaBufSource`](crate::ExternalDmaBufSource) wraps one buffer
//! presented over and over, this registers a framebuffer per slot at
//! construction and rotates between them as the producer submits. The buffers
//! come from outside — a WebGPU or Vulkan renderer, a CPU-side producer — and
//! the ring only imports them.
//!
//! The presentation decisions all live in [`RingPresenter`]: which slot is
//! pending, which is on screen, when a slot may be handed back. This type is
//! the adapter that maps a slot index to its framebuffer id and fires the
//! producer's callback.
//!
//! # Threading
//!
//! [`submit`](ExternalDmaBufRing::submit) runs on the producer's thread while
//! the scene calls `acquire` and `release` on its commit thread.

use std::time::Duration;

use drmkit_core::Device;
use drmkit_scene::{AcquiredBuffer, DamageRect, LayerBufferSource, SourceError, SourceFormat};
use drmkit_sync::SyncFence;

use crate::external::{ExternalError, ExternalPlane, ImportedFramebuffer};
use crate::presenter::{Present, RingPresenter};

/// Called when a slot leaves the screen and the producer may reuse it.
///
/// The fence, when present, is the out-fence of the commit that displaced the
/// slot: a GPU producer waits on it and re-renders with no CPU stall. `None`
/// means the call itself is the signal — either the CRTC has no
/// `OUT_FENCE_PTR`, or the producer is CPU-side and has nothing to wait on.
pub type OnRelease = Box<dyn FnMut(usize, Option<SyncFence>) + Send>;

/// A rotating ring of externally allocated DMA-BUFs.
pub struct ExternalDmaBufRing {
    slots: Vec<ImportedFramebuffer>,
    presenter: RingPresenter,
    on_release: Option<OnRelease>,
    format: SourceFormat,
}

impl std::fmt::Debug for ExternalDmaBufRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalDmaBufRing")
            .field("slots", &self.slots.len())
            .field("format", &self.format)
            .field("scanning", &self.presenter.scanning_key())
            .finish_non_exhaustive()
    }
}

impl ExternalDmaBufRing {
    /// Import every slot and register a framebuffer for each.
    ///
    /// Each slot's descriptors are duplicated here and held until the ring is
    /// dropped, so the producer may close its own at any point and the
    /// framebuffer ids stay valid across every frame.
    ///
    /// `fence_deadline` opts into the presenter's CPU pre-wait; see
    /// [`RingPresenter::acquire`] for why that excludes handing the fence to
    /// the kernel.
    ///
    /// # Errors
    ///
    /// [`ExternalError::Invalid`] for an empty slot list, and whatever the
    /// import returns for a slot the kernel refuses. Slots already imported are
    /// torn down on the way out, so a partial ring is never returned.
    pub fn create(
        device: &Device,
        format: SourceFormat,
        slots: &[&[ExternalPlane<'_>]],
        fence_deadline: Option<Duration>,
    ) -> Result<Self, ExternalError> {
        if slots.is_empty() {
            return Err(ExternalError::Invalid {
                reason: "a ring needs at least one slot",
            });
        }

        let mut imported = Vec::with_capacity(slots.len());
        for planes in slots {
            // `?` drops what is already in `imported`, and each one's own drop
            // destroys its framebuffer. A half-imported ring never escapes.
            imported.push(ImportedFramebuffer::import(device, format, planes)?);
        }

        Ok(Self {
            slots: imported,
            presenter: RingPresenter::new(fence_deadline),
            on_release: None,
            format,
        })
    }

    /// Set the callback fired when a slot leaves the screen.
    pub fn set_on_release(&mut self, on_release: OnRelease) {
        self.on_release = Some(on_release);
    }

    /// Hand in a freshly rendered slot. Producer thread.
    ///
    /// Replaces any frame not yet picked up: a producer outrunning the display
    /// should show its newest frame rather than work through a backlog.
    ///
    /// A slot index past the end is ignored. The producer owns the indices it
    /// was constructed with, and refusing loudly on its own thread — where
    /// there is no commit to fail — would give it nothing useful to do.
    pub fn submit(&self, slot: usize, fence: Option<SyncFence>, damage: &[DamageRect]) {
        if slot >= self.slots.len() {
            return;
        }
        self.presenter.submit(slot as u64, fence, damage);
    }

    /// How many slots the ring holds.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Whether a submitted frame is waiting to be picked up.
    #[must_use]
    pub fn has_fresh_frame(&self) -> bool {
        self.presenter.has_fresh_frame()
    }

    /// The slot currently on screen, if any.
    #[must_use]
    pub fn scanning_slot(&self) -> Option<usize> {
        self.presenter
            .scanning_key()
            .and_then(|key| usize::try_from(key).ok())
    }

    /// Hand a slot back to the producer.
    fn fire_release(&mut self, slot: usize, fence: Option<SyncFence>) {
        if let Some(callback) = self.on_release.as_mut() {
            callback(slot, fence);
        }
    }

    /// The framebuffer for a slot, if it still has one.
    fn fb_of(&self, key: u64) -> Option<u32> {
        // A key wider than a slot index cannot name a slot, so it resolves to
        // no framebuffer rather than being truncated into one that exists.
        self.slots.get(usize::try_from(key).ok()?)?.fb_id()
    }
}

impl LayerBufferSource for ExternalDmaBufRing {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        let decision = self.presenter.acquire();
        match decision.kind {
            Present::Fresh => {
                let fb_id = self
                    .fb_of(decision.key)
                    .ok_or(SourceError::Failed(rustix::io::Errno::INVAL))?;
                Ok(AcquiredBuffer {
                    fb_id,
                    token: decision.token,
                    acquire_fence: decision.fence,
                    damage: decision.damage,
                })
            }
            // Re-present what is already up, provided it still resolves to a
            // live framebuffer: a slot forgotten on session pause has none, and
            // committing a stale id would take the whole frame down.
            Present::Hold => {
                let fb_id = self.fb_of(decision.key).ok_or(SourceError::WouldBlock)?;
                Ok(AcquiredBuffer {
                    fb_id,
                    token: decision.token,
                    ..AcquiredBuffer::default()
                })
            }
            Present::None => Err(SourceError::WouldBlock),
        }
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.release_with_fence(acquired, None);
    }

    fn release_with_fence(&mut self, acquired: AcquiredBuffer, release_fence: Option<SyncFence>) {
        if let Some(key) = self.presenter.release(acquired.token)
            && let Ok(slot) = usize::try_from(key)
        {
            self.fire_release(slot, release_fence);
        }
    }

    fn wants_release_fence(&self) -> bool {
        // Only worth the scene asking the kernel for an out-fence if someone is
        // listening for it.
        self.on_release.is_some()
    }

    fn has_fresh_content(&self) -> bool {
        self.presenter.has_fresh_frame()
    }

    fn format(&self) -> SourceFormat {
        self.format
    }

    fn on_session_paused(&mut self) {
        // The descriptor is revoked, so the framebuffers cannot be destroyed
        // through it and their ids must not be committed either. Forgetting
        // both makes `acquire` report no frame rather than hand out an id the
        // kernel no longer knows.
        for slot in &mut self.slots {
            slot.forget();
        }
        self.presenter.reset();
    }
}
