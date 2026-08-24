// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A lazily populated pool of caller-owned DMA-BUFs, keyed by the producer.
//!
//! Where [`ExternalDmaBufRing`](crate::ExternalDmaBufRing) fixes its slot set up
//! front and the producer submits by index, this imports a buffer the first
//! time it is submitted and reuses the cached framebuffer on every later submit
//! of the same key. A stateful decoder that recycles an internal pool and hands
//! out descriptors per frame — V4L2 capture, Rockchip MPP — never tells anyone
//! its buffer set in advance, so there is nothing to build a ring from.
//!
//! It is the lazy-import half of [`DmaBufSourceCache`](crate::DmaBufSourceCache)
//! folded behind the ring's presentation machine, sharing
//! [`RingPresenter`](crate::RingPresenter) for the handoff, tokens, deferred
//! release, idle-hold and damage carry.
//!
//! # Threading
//!
//! `submit` runs on the producer's thread and performs the first-sight import,
//! so unlike the ring's it is not allocation-free and can fail. The scene calls
//! `acquire` and `release` on its commit thread. The import map and the
//! presenter's handoff are separate locks and are never held nested.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Mutex;
use std::time::Duration;

use drmkit_core::Device;
use drmkit_scene::{AcquiredBuffer, DamageRect, LayerBufferSource, SourceError, SourceFormat};
use drmkit_sync::SyncFence;

use crate::external::{ExternalPlane, ImportedFramebuffer};
use crate::presenter::{Present, RingPresenter};
use crate::ring::OnRelease;

/// One imported buffer.
struct PoolSlot {
    imported: ImportedFramebuffer,
    /// Set by [`ExternalDmaBufPool::reset_generation`]: the buffer belongs to a
    /// configuration the producer has moved on from, and is torn down once no
    /// commit still references it.
    retiring: bool,
}

/// A pool of externally allocated DMA-BUFs, imported on first sight.
pub struct ExternalDmaBufPool {
    slots: Mutex<HashMap<u64, PoolSlot>>,
    presenter: RingPresenter,
    format: Mutex<SourceFormat>,
    on_release: Option<OnRelease>,
}

impl std::fmt::Debug for ExternalDmaBufPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalDmaBufPool")
            .field("cached", &self.cached_count())
            .field("scanning", &self.presenter.scanning_key())
            .finish_non_exhaustive()
    }
}

impl ExternalDmaBufPool {
    /// An empty pool. Buffers arrive through [`submit`](Self::submit).
    ///
    /// `fence_deadline` opts into the presenter's CPU pre-wait; see
    /// [`RingPresenter::acquire`](crate::RingPresenter::acquire) for why that
    /// excludes handing the fence to the kernel.
    #[must_use]
    pub fn new(format: SourceFormat, fence_deadline: Option<Duration>) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            presenter: RingPresenter::new(fence_deadline),
            format: Mutex::new(format),
            on_release: None,
        }
    }

    /// Set the callback fired when a buffer leaves the screen.
    pub fn set_on_release(&mut self, on_release: OnRelease) {
        self.on_release = Some(on_release);
    }

    /// Hand in the buffer to scan out next. Producer thread.
    ///
    /// The first submit of a key imports its planes and caches the framebuffer;
    /// later submits of the same key reuse it and ignore `planes` entirely.
    ///
    /// `device` is a parameter rather than something the pool holds because the
    /// scene owns its sources for `'static` and a borrowed device cannot live
    /// there. Upstream keeps a raw descriptor instead; passing the device says
    /// the same thing without a descriptor outliving what opened it.
    ///
    /// **A failed import skips the frame rather than reporting.** The producer
    /// has nothing useful to do about it on its own thread, and the presenter
    /// holds the last good buffer — a frozen layer beats a blank one. Returns
    /// whether the frame was taken, for a caller that wants to count.
    pub fn submit(
        &self,
        device: &Device,
        key: u64,
        planes: &[ExternalPlane<'_>],
        fence: Option<SyncFence>,
        damage: &[DamageRect],
    ) -> bool {
        let format = *self.lock_format();
        {
            let mut slots = self.lock_slots();
            if let Entry::Vacant(entry) = slots.entry(key) {
                // First sight of this key. A later submit of it reuses this
                // import and ignores `planes` entirely.
                let Ok(imported) = ImportedFramebuffer::import(device, format, planes) else {
                    return false;
                };
                entry.insert(PoolSlot {
                    imported,
                    retiring: false,
                });
            }
        }
        // The two locks are never held together: the import map is released
        // before the presenter's handoff is taken, so there is no order to get
        // wrong between the producer and commit threads.
        self.presenter.submit(key, fence, damage);
        true
    }

    /// How many buffers are currently imported.
    #[must_use]
    pub fn cached_count(&self) -> usize {
        self.lock_slots().len()
    }

    /// Move to a new buffer generation — a decoder resolution or format change.
    ///
    /// Future submits import under `format`, and every buffer cached now is
    /// marked for retirement: each is torn down on a later acquire once no
    /// in-flight commit still references it. Tearing them down here instead
    /// would destroy a framebuffer the kernel is still scanning out.
    ///
    /// The producer must submit the new generation under **fresh keys**. A
    /// reallocated pool has new buffer identities, and reusing an old key
    /// before its buffer retires would hand back the previous generation's
    /// framebuffer at the previous generation's geometry.
    pub fn reset_generation(&self, format: SourceFormat) {
        *self.lock_format() = format;
        for slot in self.lock_slots().values_mut() {
            slot.retiring = true;
        }
    }

    /// Drop retiring buffers no commit still holds.
    fn sweep(&mut self) {
        // One pass under one guard. Deciding and then acting under separate
        // locks would leave a window where a buffer became referenced between
        // the two and was torn down anyway.
        let Self {
            slots, presenter, ..
        } = self;
        let slots = slots
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slots.retain(|key, slot| !slot.retiring || presenter.is_referenced(*key));
    }

    fn fb_of(&self, key: u64) -> Option<u32> {
        self.lock_slots().get(&key)?.imported.fb_id()
    }

    fn lock_slots(&self) -> std::sync::MutexGuard<'_, HashMap<u64, PoolSlot>> {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_format(&self) -> std::sync::MutexGuard<'_, SourceFormat> {
        self.format
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl LayerBufferSource for ExternalDmaBufPool {
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        let decision = self.presenter.acquire();
        let result = match decision.kind {
            Present::Fresh => self.fb_of(decision.key).map_or(
                Err(SourceError::Failed(rustix::io::Errno::INVAL)),
                |fb_id| {
                    Ok(AcquiredBuffer {
                        fb_id,
                        token: decision.token,
                        acquire_fence: decision.fence,
                        damage: decision.damage,
                    })
                },
            ),
            // Re-present what is up, provided it still resolves: a buffer
            // retired or forgotten between frames has no framebuffer, and
            // committing a stale id takes the whole frame down.
            Present::Hold => {
                self.fb_of(decision.key)
                    .map_or(Err(SourceError::WouldBlock), |fb_id| {
                        Ok(AcquiredBuffer {
                            fb_id,
                            token: decision.token,
                            ..AcquiredBuffer::default()
                        })
                    })
            }
            Present::None => Err(SourceError::WouldBlock),
        };

        // After the decision, so a buffer this frame just took is referenced
        // and survives the sweep.
        self.sweep();
        result
    }

    fn release(&mut self, acquired: AcquiredBuffer) {
        self.release_with_fence(acquired, None);
    }

    fn release_with_fence(&mut self, acquired: AcquiredBuffer, release_fence: Option<SyncFence>) {
        if let Some(key) = self.presenter.release(acquired.token) {
            if let Some(callback) = self.on_release.as_mut() {
                // The pool's keys are the producer's own, so they are handed
                // back as-is rather than as an index into anything.
                callback(usize::try_from(key).unwrap_or(usize::MAX), release_fence);
            }
            self.sweep();
        }
    }

    fn wants_release_fence(&self) -> bool {
        self.on_release.is_some()
    }

    fn has_fresh_content(&self) -> bool {
        self.presenter.has_fresh_frame()
    }

    fn format(&self) -> SourceFormat {
        *self.lock_format()
    }

    fn on_session_paused(&mut self) {
        // Every framebuffer id belongs to a descriptor that is about to be
        // revoked. Dropping the imports without ioctls leaves the pool empty,
        // so the producer's next submit re-imports against the live device
        // rather than the pool handing out ids the kernel has forgotten.
        for slot in self.lock_slots().values_mut() {
            slot.imported.forget();
        }
        self.lock_slots().clear();
        self.presenter.reset();
    }
}
