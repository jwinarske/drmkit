// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The scanout slot pool, and the damage bookkeeping that makes partial
//! repaint correct.

use std::collections::VecDeque;

/// A damage rectangle, in buffer pixels.
///
/// Deliberately not `drmkit_planes::Rect`. That one is a plane's source or
/// destination rectangle in CRTC coordinates; this is a region of a buffer the
/// producer owns, and the two spaces coincide only when a layer happens to be
/// unscaled and at the origin. Sharing the type would invite passing one where
/// the other is meant, which is a bug no compiler would catch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// What a reused slot must repaint before its contents are a correct frame.
///
/// An enum rather than the reference's `{ bool full; vector<Rect> region; }`,
/// because that struct can hold a full repaint *and* a region, and a caller
/// reading the region without checking the flag under-paints. Here the region
/// only exists when it is the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repaint {
    /// Repaint the whole buffer: the slot has never been presented, or its
    /// damage history has scrolled away, or the union grew past the cap.
    Full,
    /// Repaint exactly this union of damage since the slot was last presented.
    ///
    /// In present order, oldest first, and not merged or deduplicated -- the
    /// producer knows more about its own cost model than this does.
    Region(Vec<Rect>),
}

/// A slot handed to the producer for the next frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    /// Index into the producer's own buffer array.
    pub slot: usize,
    /// The ring grew to make this slot, so no buffer exists for it yet.
    pub fresh: bool,
    /// What the producer must draw to make this slot a correct frame.
    pub repaint: Repaint,
}

/// Where a slot is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Nothing holds it; `acquire` may hand it out.
    Free,
    /// Leased to the producer, which is drawing into it.
    InFlight,
    /// On screen. Never handed out, never released.
    Scanning,
    /// Replaced, but its flip has not landed, so the display engine may still
    /// be reading it.
    PendingRelease,
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    state: State,
    /// Present sequence when this slot was last presented. Zero means never,
    /// which is why sequences start at one.
    last_present: u64,
}

/// A producer-agnostic pool of scanout slots with buffer age and damage
/// accumulation.
///
/// The ring owns slot *lifecycle* and staleness; the producer owns the actual
/// buffers and indexes them by slot. That split is what lets the same ring sit
/// under a dumb buffer, a GBM surface or a Vulkan swapchain without knowing
/// which.
///
/// # What it is for
///
/// Redrawing a whole frame when three pixels changed wastes bandwidth, and on
/// a memory-bound part that is the difference between hitting vblank and
/// missing it. Partial repaint avoids it, but only if the producer knows how
/// stale the buffer it has just been handed is -- a buffer two frames old needs
/// everything that changed in those two frames repainted, not just this
/// frame's damage. That is the `EGL_EXT_buffer_age` contract, and this is the
/// bookkeeping behind it.
///
/// Getting it wrong under-paints: the frame is composed of pixels from
/// different moments, and the error persists until something forces a full
/// repaint. So every path that cannot prove what a slot needs returns
/// [`Repaint::Full`].
///
/// # Threading
///
/// Not synchronised, by design. Drive `acquire`, `present` and `release` from
/// the one thread the producer renders on -- the present loop. `&mut self` on
/// the mutating methods makes sharing it across threads a compile error rather
/// than a race.
#[derive(Debug)]
pub struct BufferRing {
    slots: Vec<Slot>,
    /// Per-frame damage, oldest first. `damage[0]` belongs to present sequence
    /// `base_seq + 1`, and the back to `present_seq`.
    damage: VecDeque<Vec<Rect>>,
    /// Last committed present sequence. Zero means nothing presented yet.
    present_seq: u64,
    /// The sequence just before `damage.front()`, so history that has scrolled
    /// away is detectable rather than silently mis-indexed.
    base_seq: u64,
    max_slots: usize,
    max_damage_rects: usize,
}

impl BufferRing {
    /// How deep a ring grows, and how many rectangles a union may hold, unless
    /// the caller says otherwise.
    pub const DEFAULT_MAX_SLOTS: usize = 3;
    /// See [`BufferRing::new`] for what happens when a union exceeds this.
    pub const DEFAULT_MAX_DAMAGE_RECTS: usize = 16;

    /// A ring that grows to `max_slots`, with unions capped at
    /// `max_damage_rects`.
    ///
    /// `max_slots` is clamped to at least one: a ring with no slots can never
    /// hand one out, and a caller asking for zero has made an arithmetic
    /// mistake rather than a request.
    ///
    /// The rect cap is a bandwidth-versus-bookkeeping knob. A union that
    /// exceeds it collapses to [`Repaint::Full`], on the reasoning that
    /// scattered small rectangles cost more in per-rect overhead than the
    /// bandwidth they save.
    #[must_use]
    pub fn new(max_slots: usize, max_damage_rects: usize) -> Self {
        Self {
            slots: Vec::new(),
            damage: VecDeque::new(),
            present_seq: 0,
            base_seq: 0,
            max_slots: max_slots.max(1),
            max_damage_rects,
        }
    }

    /// How many slots the ring has grown so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the ring has grown any slots yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The cap this ring grows to.
    #[must_use]
    pub const fn max_slots(&self) -> usize {
        self.max_slots
    }

    /// How many slots the producer is currently drawing into.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.state == State::InFlight)
            .count()
    }

    /// How many presents have happened since this slot was last presented.
    ///
    /// Zero means it has never been presented and its contents are undefined --
    /// not that it is current. The two are distinguished by
    /// [`Repaint::Full`] coming back from [`acquire`](Self::acquire), which is
    /// the answer that actually matters to a producer.
    #[must_use]
    pub fn age(&self, slot: usize) -> u32 {
        match self.slots.get(slot) {
            Some(s) if s.last_present != 0 => {
                u32::try_from(self.present_seq - s.last_present).unwrap_or(u32::MAX)
            }
            _ => 0,
        }
    }

    /// Take a slot for the next frame, or `None` if every one is busy.
    ///
    /// Prefers the freshest free slot, because freshest means least to
    /// repaint. Grows the ring if nothing is free and it is below capacity.
    /// Never returns the scanning slot.
    ///
    /// `None` is not an error: it means the producer is ahead of the display
    /// and should retry after the next vblank.
    pub fn acquire(&mut self) -> Option<Lease> {
        let best = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.state == State::Free)
            .max_by_key(|(_, slot)| slot.last_present)
            .map(|(index, _)| index);

        if let Some(index) = best {
            self.slots[index].state = State::InFlight;
            return Some(Lease {
                slot: index,
                fresh: false,
                repaint: self.repaint_for(self.slots[index].last_present),
            });
        }

        if self.slots.len() < self.max_slots {
            let index = self.slots.len();
            self.slots.push(Slot {
                state: State::InFlight,
                last_present: 0,
            });
            return Some(Lease {
                slot: index,
                fresh: true,
                repaint: Repaint::Full,
            });
        }

        None
    }

    /// Commit a slot as the new scanout buffer.
    ///
    /// `frame_damage` is what changed in this frame against the previous one --
    /// not against whatever was in this slot, which is what the ring works out
    /// for itself.
    ///
    /// The outgoing scanning slot becomes releasable rather than free: its flip
    /// has not landed, so the display engine may still be reading it. Freeing
    /// it here is invariant 1's mistake in a different place.
    pub fn present(&mut self, slot: usize, frame_damage: &[Rect]) {
        if slot >= self.slots.len() {
            return;
        }
        for s in &mut self.slots {
            if s.state == State::Scanning {
                s.state = State::PendingRelease;
            }
        }
        self.present_seq += 1;
        self.damage.push_back(frame_damage.to_vec());
        // Only as much history as the deepest reusable slot could need. Older
        // frames falling off is safe: a slot that stale gets a full repaint.
        while self.damage.len() > self.max_slots {
            self.damage.pop_front();
            self.base_seq += 1;
        }
        self.slots[slot].state = State::Scanning;
        self.slots[slot].last_present = self.present_seq;
    }

    /// Return a replaced slot to the free pool.
    ///
    /// Call it once the flip that replaced this slot has landed, or once its
    /// render has been abandoned. The scanning slot is ignored rather than
    /// freed -- releasing what is on screen is not a request this can honour.
    pub fn release(&mut self, slot: usize) {
        if let Some(s) = self.slots.get_mut(slot)
            && s.state != State::Scanning
        {
            s.state = State::Free;
        }
    }

    /// What a slot last presented at `last_present` must repaint.
    fn repaint_for(&self, last_present: u64) -> Repaint {
        // Never presented, or its damage has scrolled out of history.
        if last_present == 0 || last_present < self.base_seq {
            return Repaint::Full;
        }
        // Sequence S sits at index (S - base_seq - 1), so the first frame this
        // slot missed -- last_present + 1 -- is at (last_present - base_seq).
        let from = usize::try_from(last_present - self.base_seq).unwrap_or(usize::MAX);
        let mut region = Vec::new();
        for frame in self.damage.iter().skip(from) {
            for rect in frame {
                region.push(*rect);
                if region.len() > self.max_damage_rects {
                    return Repaint::Full;
                }
            }
        }
        Repaint::Region(region)
    }
}

impl Default for BufferRing {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_SLOTS, Self::DEFAULT_MAX_DAMAGE_RECTS)
    }
}
