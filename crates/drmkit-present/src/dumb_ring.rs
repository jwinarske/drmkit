// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! A multi-buffered CPU source, and the canonical [`BufferRing`] consumer.

use drmkit_core::Device;
use drmkit_dumb::{Buffer, Config, DumbError, MapAccess, Mapping};
use drmkit_scene::{
    AcquiredBuffer, BindingModel, DamageRect, LayerBufferSource, SourceError, SourceFormat,
};

use crate::{BufferRing, Rect, Repaint};

/// What went wrong painting a frame.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PaintError {
    /// Every slot is busy. Retry after the next flip completes.
    ///
    /// Flow control, not a failure: the producer is ahead of the display.
    #[error("every ring slot is busy")]
    WouldBlock,

    /// A slot's buffer could not be allocated.
    #[error("allocating a ring slot: {0}")]
    Allocate(#[from] DumbError),

    /// The format's bits-per-pixel is not one this can size an allocation
    /// from.
    #[error("no known bits-per-pixel for fourcc {0:#010x}")]
    UnknownFormat(u32),
}

/// A ring of CPU-mapped dumb buffers, presented as a scene layer's source.
///
/// This is what turns [`BufferRing`]'s bookkeeping into something a producer
/// can use: [`paint`](Self::paint) hands a callback the leased buffer's
/// mapping *and the region it must repaint*, and whatever the callback says it
/// changed becomes the next commit's `FB_DAMAGE_CLIPS`.
///
/// # Why it lives here and not in `drmkit-scene-sources`
///
/// It depends on [`BufferRing`], and the scene must not depend on the present
/// spine — the producer seam points the other way. It is still an ordinary
/// [`LayerBufferSource`], so a scene layer holds one directly without knowing
/// where it came from.
///
/// # The contract, for a CPU producer
///
/// Once per frame, call `paint`. The callback gets the stale region: [`Repaint::Full`]
/// for a slot that is fresh or too old to track, otherwise the union of damage
/// since *this slot* was last on screen — which is not the same as what changed
/// this frame, and is the whole reason the ring exists. Paint at least that
/// much, return what you actually touched, and the scene's next commit scans
/// out the slot and reports exactly that as its damage.
///
/// Under-reporting leaves stale pixels on screen. Over-reporting costs
/// bandwidth and nothing else, which is why every path that cannot prove the
/// stale region returns `Full`.
///
/// # Threading
///
/// Single-threaded: drive `paint` and the scene commit from one loop.
pub struct DumbRingSource {
    format: SourceFormat,
    bpp: u32,
    ring: BufferRing,
    buffers: Vec<Option<Buffer>>,
    /// The slot `paint` committed, waiting for the scene to acquire it. Cleared
    /// on acquire, so every commit needs a fresh paint.
    pending: Option<(usize, Vec<DamageRect>)>,
}

impl DumbRingSource {
    /// A ring of up to `max_slots` linear dumb buffers.
    ///
    /// Nothing is allocated here. Slots are allocated by [`paint`](Self::paint)
    /// as the ring grows under contention, which on a 4K display is the
    /// difference between reserving one buffer and reserving three.
    ///
    /// # Errors
    ///
    /// [`PaintError::UnknownFormat`] if `fourcc`'s bits-per-pixel is not known,
    /// since that is what sizes the allocation.
    pub fn new(width: u32, height: u32, fourcc: u32, max_slots: usize) -> Result<Self, PaintError> {
        let bpp = drmkit_fmt::format_bpp(fourcc);
        if bpp == 0 {
            return Err(PaintError::UnknownFormat(fourcc));
        }
        Ok(Self {
            format: SourceFormat {
                fourcc,
                modifier: drmkit_fmt::Modifier::LINEAR.0,
                width,
                height,
            },
            bpp,
            ring: BufferRing::new(max_slots, BufferRing::DEFAULT_MAX_DAMAGE_RECTS),
            buffers: Vec::new(),
            pending: None,
        })
    }

    /// Paint the next frame.
    ///
    /// The callback receives the leased buffer's writable mapping and the
    /// region it must repaint, and returns the region it actually changed, in
    /// buffer pixels.
    ///
    /// `device` is a parameter rather than something the source holds. The
    /// reference keeps a raw `const drm::Device*` and swaps it on session
    /// resume; storing a borrow here would put a lifetime on the type and into
    /// every scene that holds one, and storing a pointer is the thing Rust is
    /// for avoiding. Passing it per call also means there is no stale device to
    /// swap when a session resumes — the next paint simply gets the new one.
    ///
    /// # Errors
    ///
    /// [`PaintError::WouldBlock`] when every slot is busy, which is flow
    /// control rather than failure. Otherwise whatever allocating a slot
    /// failed with.
    pub fn paint<F>(&mut self, device: &Device, paint: F) -> Result<(), PaintError>
    where
        F: FnOnce(&mut Mapping<'_>, &Repaint) -> Vec<Rect>,
    {
        let lease = self.ring.acquire().ok_or(PaintError::WouldBlock)?;
        let buffer = self.ensure_slot(device, lease.slot)?;
        let painted = {
            let mut mapping = buffer.map(MapAccess::Write);
            paint(&mut mapping, &lease.repaint)
        };

        let damage = painted
            .iter()
            .map(|r| DamageRect {
                x: r.x,
                y: r.y,
                w: r.width,
                h: r.height,
            })
            .collect();
        // The ring is told what changed *this frame*; it works out for itself
        // what any other slot therefore owes.
        self.ring.present(lease.slot, &painted);
        self.pending = Some((lease.slot, damage));
        Ok(())
    }

    /// How many slots have been allocated so far.
    #[must_use]
    pub fn allocated(&self) -> usize {
        self.buffers.iter().filter(|b| b.is_some()).count()
    }

    /// The slot's buffer, allocating it if this is its first use.
    ///
    /// Returns the buffer rather than `()` so the caller has no unreachable
    /// unwrap to justify.
    fn ensure_slot(&mut self, device: &Device, slot: usize) -> Result<&mut Buffer, PaintError> {
        if self.buffers.len() <= slot {
            self.buffers.resize_with(slot + 1, || None);
        }
        if self.buffers[slot].is_none() {
            self.buffers[slot] = Some(Self::allocate(device, &self.format, self.bpp)?);
        }
        Ok(self.buffers[slot]
            .as_mut()
            .unwrap_or_else(|| unreachable!("just allocated")))
    }

    /// One slot's buffer.
    fn allocate(device: &Device, format: &SourceFormat, bpp: u32) -> Result<Buffer, PaintError> {
        let buffer = Buffer::create(
            device,
            &Config {
                width: format.width,
                height: format.height,
                fourcc: format.fourcc,
                bpp,
                add_fb: true,
            },
        )?;
        Ok(buffer)
    }
}

impl LayerBufferSource for DumbRingSource {
    /// Hand the scene the slot the last `paint` committed.
    ///
    /// [`SourceError::WouldBlock`] when nothing has been painted since the last
    /// acquire — the same signal a camera gives before its first frame, and
    /// handled the same way: the layer keeps what it had rather than the frame
    /// failing.
    fn acquire(&mut self) -> Result<AcquiredBuffer, SourceError> {
        let (slot, damage) = self.pending.take().ok_or(SourceError::WouldBlock)?;
        let fb_id = self
            .buffers
            .get(slot)
            .and_then(Option::as_ref)
            .and_then(Buffer::fb_id)
            .ok_or(SourceError::WouldBlock)?;
        Ok(AcquiredBuffer {
            fb_id,
            token: u64::try_from(slot).unwrap_or(u64::MAX),
            acquire_fence: None,
            damage,
        })
    }

    /// Return a slot to the ring once its flip has landed.
    fn release(&mut self, acquired: AcquiredBuffer) {
        // A token that does not fit is not a slot this ring handed out, and
        // `release` ignores an out-of-range one.
        let slot = usize::try_from(acquired.token).unwrap_or(usize::MAX);
        self.ring.release(slot);
    }

    fn binding_model(&self) -> BindingModel {
        BindingModel::SceneSubmitsFbId
    }

    fn format(&self) -> SourceFormat {
        self.format
    }

    /// Drop every buffer without touching the dead descriptor.
    ///
    /// The GEM handles and framebuffer ids belonged to it and the kernel
    /// reclaims them when it closes; issuing ioctls against it would fail and
    /// change nothing.
    fn on_session_paused(&mut self) {
        for slot in &mut self.buffers {
            if let Some(buffer) = slot.as_mut() {
                buffer.forget();
            }
            *slot = None;
        }
        self.pending = None;
    }

    /// Nothing to do: the next `paint` allocates against the device it is
    /// given.
    ///
    /// The reference re-points its stored device here. Not holding one is what
    /// makes this empty.
    fn on_session_resumed(&mut self, _device: &Device) -> Result<(), SourceError> {
        Ok(())
    }
}
