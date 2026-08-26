// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The presentation state machine shared by the externally-fed sources.
//!
//! Tracks which buffer is pending and which is on screen, hands out the
//! monotonic tokens the scene's deferred-release protocol keys on, carries
//! damage forward across frames it drops, and enforces the optional fence
//! deadline.
//!
//! It owns **no buffer storage**. A buffer is only a [`SlotKey`] — a dense
//! index for a fixed ring, a stable per-buffer key for a pool — so the same
//! machine drives both without knowing which it is. It also means all of it is
//! testable with no device anywhere.
//!
//! # Threading
//!
//! `submit` runs on the producer's thread while the scene calls `acquire` and
//! `release` on its commit thread. Only the pending handoff is shared, so only
//! that is behind the lock; the scanning bookkeeping is commit-thread-only.

use std::sync::Mutex;
use std::time::Duration;

use drmkit_scene::DamageRect;
use drmkit_sync::SyncFence;

/// Opaque buffer identity: a slot index, or a pool's own key.
pub type SlotKey = u64;

/// How many damage rectangles a submit can carry.
///
/// Fixed so `submit` allocates nothing on the producer's thread. An over-cap
/// frame degrades to whole-frame rather than truncating — a short list would
/// under-report the dirty area, and the parts left out would keep whatever the
/// previous frame had.
pub const MAX_DAMAGE: usize = 16;

/// What the commit thread should do this vblank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Present {
    /// Nothing to present; the adapter returns `WouldBlock`.
    None,
    /// A newly acquired frame, under a new token.
    Fresh,
    /// Re-present the buffer already on screen, under its existing token.
    Hold,
}

/// The commit thread's decision.
#[derive(Debug)]
pub struct Acquired {
    /// What to do.
    pub kind: Present,
    /// Which buffer, for `Fresh` and `Hold`.
    pub key: SlotKey,
    /// The token the release protocol keys on.
    pub token: u64,
    /// The producer's render fence. `Fresh` only, and never set when a
    /// deadline is configured — see [`RingPresenter::acquire`].
    pub fence: Option<SyncFence>,
    /// Dirty regions, `Fresh` only. Empty means whole-frame.
    pub damage: Vec<DamageRect>,
}

#[derive(Default)]
struct Pending {
    key: Option<SlotKey>,
    fence: Option<SyncFence>,
    damage: Vec<DamageRect>,
}

/// Tracks pending, scanning and in-flight buffers across frames.
pub struct RingPresenter {
    pending: Mutex<Pending>,
    scanning_key: Option<SlotKey>,
    scanning_token: u64,
    next_token: u64,
    /// Tokens handed out and not yet retired, with the buffer each names.
    outstanding: Vec<(u64, SlotKey)>,
    fence_deadline: Option<Duration>,
    /// Damage from frames dropped on a deadline miss, owed to the next frame
    /// that is actually presented.
    carried_damage: Vec<DamageRect>,
    /// Whether what was dropped was a *whole-frame* update.
    ///
    /// Separate from `carried_damage` being empty, which is ambiguous: an
    /// empty list means both "nothing was dropped" and "everything was". Only
    /// the second forces the next frame to whole-frame, and without this flag
    /// the two are indistinguishable by the time that frame arrives.
    carried_whole_frame: bool,
}

impl RingPresenter {
    /// A presenter with an optional fence deadline.
    ///
    /// With a deadline set, a producer's fence is CPU-waited here and **never**
    /// handed to the kernel — see [`acquire`](Self::acquire) for why those are
    /// mutually exclusive.
    #[must_use]
    pub fn new(fence_deadline: Option<Duration>) -> Self {
        Self {
            pending: Mutex::new(Pending::default()),
            scanning_key: None,
            scanning_token: 0,
            next_token: 0,
            outstanding: Vec::new(),
            fence_deadline,
            carried_damage: Vec::new(),
            carried_whole_frame: false,
        }
    }

    /// The configured fence deadline, if any.
    #[must_use]
    pub const fn fence_deadline(&self) -> Option<Duration> {
        self.fence_deadline
    }

    /// Hand in a freshly rendered buffer. Producer thread.
    ///
    /// Replaces any previous pending frame rather than queueing: a producer
    /// that outruns the display should show its newest frame, not work through
    /// a backlog. `damage` replaces too, and an over-cap list becomes
    /// whole-frame.
    pub fn submit(&self, key: SlotKey, fence: Option<SyncFence>, damage: &[DamageRect]) {
        let mut pending = self.lock();
        pending.key = Some(key);
        pending.fence = fence;
        pending.damage.clear();
        if damage.len() <= MAX_DAMAGE {
            pending.damage.extend_from_slice(damage);
        }
    }

    /// Whether a submitted frame is waiting.
    #[must_use]
    pub fn has_fresh_frame(&self) -> bool {
        self.lock().key.is_some()
    }

    /// The buffer currently on screen, if any.
    #[must_use]
    pub const fn scanning_key(&self) -> Option<SlotKey> {
        self.scanning_key
    }

    /// Whether `key` is still on screen or named by an unretired token.
    ///
    /// A pool consults this before evicting a cached buffer: one the kernel may
    /// still be scanning out must not have its import torn down.
    #[must_use]
    pub fn is_referenced(&self, key: SlotKey) -> bool {
        self.scanning_key == Some(key) || self.outstanding.iter().any(|(_, k)| *k == key)
    }

    /// Advance one vblank. Commit thread.
    ///
    /// A configured deadline makes the fence a CPU pre-wait here, and the fence
    /// is then dropped rather than also being handed to `IN_FENCE_FD`. The two
    /// are mutually exclusive on purpose: an in-fence the kernel is already
    /// holding that never signals wedges the entire CRTC's pipeline, not just
    /// this layer. Waiting here instead keeps a stuck producer's blast radius
    /// to its own frame.
    ///
    /// Missing the deadline drops that frame and holds the last good one —
    /// frozen but alive, rather than blank. Its damage is carried forward,
    /// because those regions were never presented.
    pub fn acquire(&mut self) -> Acquired {
        let (mut key, mut fence, mut damage) = {
            let mut pending = self.lock();
            if pending.key.is_some() {
                (
                    pending.key.take(),
                    pending.fence.take(),
                    std::mem::take(&mut pending.damage),
                )
            } else {
                (None, None, Vec::new())
            }
        };

        if let (Some(deadline), true, Some(f)) = (self.fence_deadline, key.is_some(), fence.take())
        {
            // A sub-millisecond deadline rounds up: the wait polls in whole
            // milliseconds, so truncating 500µs to zero would never wait at
            // all. An explicit zero stays zero.
            let waited = if deadline.is_zero() {
                Duration::ZERO
            } else {
                deadline.max(Duration::from_millis(1))
            };
            if f.wait(waited).is_err() {
                self.carry(&damage);
                damage.clear();
                key = None;
            }
        }

        if let Some(key) = key {
            self.scanning_key = Some(key);
            self.next_token = self.next_token.wrapping_add(1);
            if self.next_token == 0 {
                // Zero is the "never submitted" sentinel, so skip it on wrap
                // rather than hand out a token release() will ignore.
                self.next_token = 1;
            }
            self.scanning_token = self.next_token;
            self.outstanding.push((self.scanning_token, key));

            let mut out = std::mem::take(&mut self.carried_damage);
            let dropped_whole_frame = std::mem::take(&mut self.carried_whole_frame);
            if dropped_whole_frame || damage.is_empty() || out.len() + damage.len() > MAX_DAMAGE {
                // A dropped whole-frame, this frame's own whole-frame, or too
                // many rectangles between them: all three degrade to
                // whole-frame rather than under-reporting, which would leave
                // stale pixels on screen.
                out.clear();
            } else {
                out.extend_from_slice(&damage);
            }

            return Acquired {
                kind: Present::Fresh,
                key,
                token: self.scanning_token,
                fence,
                damage: out,
            };
        }

        // Nothing fresh and ready. Re-present what is on screen under the
        // *same* token, so the held buffer is not mistaken for a superseded one
        // and released out from under the display. No new outstanding entry —
        // the live token already has one.
        if let Some(key) = self.scanning_key {
            return Acquired {
                kind: Present::Hold,
                key,
                token: self.scanning_token,
                fence: None,
                damage: Vec::new(),
            };
        }

        Acquired {
            kind: Present::None,
            key: 0,
            token: 0,
            fence: None,
            damage: Vec::new(),
        }
    }

    /// Retire the acquisition that carried `token`.
    ///
    /// Returns the buffer to hand back, or `None` when there is nothing to
    /// release: the sentinel token, a buffer still on screen, or a token
    /// already retired.
    pub fn release(&mut self, token: u64) -> Option<SlotKey> {
        if token == 0 {
            return None;
        }
        // A buffer holding the live token is still on screen — including
        // through any number of idle holds, which all share it. Telling the
        // producer it is free would race it into overwriting a buffer the
        // kernel is scanning out.
        if token == self.scanning_token {
            return None;
        }
        // Erase on fire, so the several holds that share one token collapse to
        // a single release and a token that is absent was already retired.
        let at = self.outstanding.iter().position(|(t, _)| *t == token)?;
        Some(self.outstanding.remove(at).1)
    }

    /// Drop all pending, in-flight and scanning state.
    ///
    /// For a session resume, which re-imports every buffer and restarts
    /// presentation from scratch.
    pub fn reset(&mut self) {
        let mut pending = self.lock();
        *pending = Pending::default();
        drop(pending);
        self.scanning_key = None;
        self.scanning_token = 0;
        self.outstanding.clear();
        self.carried_damage.clear();
        self.carried_whole_frame = false;
    }

    /// Accumulate damage owed by a dropped frame.
    fn carry(&mut self, damage: &[DamageRect]) {
        if damage.is_empty() || self.carried_damage.len() + damage.len() > MAX_DAMAGE {
            // A dropped whole-frame stays whole-frame, and an overflowing
            // accumulation degrades to one: reporting less than was dirtied
            // leaves stale pixels on screen.
            //
            // The flag is what carries that across to the next frame. Clearing
            // the list alone loses it -- an empty carry is indistinguishable
            // from nothing having been dropped, and the next frame would go out
            // with only its own rects while the rest of the screen stayed stale.
            self.carried_damage.clear();
            self.carried_damage.shrink_to_fit();
            self.carried_whole_frame = true;
            return;
        }
        self.carried_damage.extend_from_slice(damage);
    }

    /// The pending state, recovering from a poisoned lock.
    ///
    /// A producer thread that panicked mid-submit leaves the handoff possibly
    /// half-written, but every field is replaced wholesale by the next submit
    /// and the worst case is one stale frame. Refusing to present again for the
    /// rest of the session would be the larger failure.
    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32) -> DamageRect {
        DamageRect { x, y, w: 8, h: 8 }
    }

    /// A dropped whole-frame update forces the next frame to whole-frame.
    ///
    /// Empty damage means *everything changed*, and no rect list can say that.
    /// So a dropped whole-frame cannot be unioned into the next frame's rects
    /// -- carrying nothing would silently downgrade it to "only what the next
    /// frame touched", leaving the rest of the screen showing the frame before
    /// the one that was dropped.
    ///
    /// Found by the vkms parity port of upstream's
    /// `DroppedWholeFrameForcesWholeFrameCarry`. Pinned here too because it is
    /// pure logic: it needs a missed deadline, not a card.
    #[test]
    fn a_dropped_whole_frame_forces_the_next_frame_to_whole_frame() {
        let mut p = RingPresenter::new(Some(Duration::from_millis(1)));
        p.submit(0, None, &[]);
        let first = p.acquire();
        assert_eq!(first.kind, Present::Fresh);
        p.release(first.token);

        // A frame behind a fence that never signals, declaring whole-frame.
        let (read, _write) = std::io::pipe().expect("a pipe stands in for a fence");
        let fence = SyncFence::import(std::os::fd::AsFd::as_fd(&read)).expect("import");
        p.submit(1, Some(fence), &[]);
        let held = p.acquire();
        assert_eq!(held.kind, Present::Hold, "the deadline miss holds");
        p.release(held.token);

        // The next frame declares a small region; the drop must override it.
        p.submit(1, None, &[rect(5, 5)]);
        let advanced = p.acquire();
        assert_eq!(advanced.kind, Present::Fresh);
        assert!(
            advanced.damage.is_empty(),
            "a rect list here leaves everything outside it stale"
        );
    }

    /// A dropped *partial* frame is unioned in, and cleared afterwards.
    ///
    /// The counterpart to the above: a dropped frame that named its regions
    /// still has them stale on screen, so they ride along with the next frame
    /// -- once. Carrying them further would over-report every frame after.
    #[test]
    fn a_dropped_partial_frame_is_carried_into_the_next_frame_only() {
        let mut p = RingPresenter::new(Some(Duration::from_millis(1)));
        p.submit(0, None, &[]);
        let token = p.acquire().token;
        p.release(token);

        let (read, _write) = std::io::pipe().expect("a pipe stands in for a fence");
        let fence = SyncFence::import(std::os::fd::AsFd::as_fd(&read)).expect("import");
        p.submit(1, Some(fence), &[rect(1, 1)]);
        let held = p.acquire();
        assert_eq!(held.kind, Present::Hold);
        p.release(held.token);

        p.submit(1, None, &[rect(5, 5)]);
        let advanced = p.acquire();
        assert_eq!(
            advanced.damage,
            vec![rect(1, 1), rect(5, 5)],
            "the dropped region is still stale, so it repaints with this frame"
        );
        p.release(advanced.token);

        p.submit(0, None, &[rect(5, 5)]);
        let after = p.acquire();
        assert_eq!(
            after.damage,
            vec![rect(5, 5)],
            "the carry is spent; repeating it would over-report from here on"
        );
    }

    /// Nothing submitted, nothing to present.
    #[test]
    fn a_presenter_with_no_frame_yet_presents_nothing() {
        let mut p = RingPresenter::new(None);
        let out = p.acquire();
        assert_eq!(out.kind, Present::None);
        assert_eq!(out.token, 0, "the sentinel token, which release ignores");
        assert!(!p.has_fresh_frame());
    }

    /// A submitted frame is presented once, under a fresh token.
    #[test]
    fn a_submitted_frame_is_presented_once() {
        let mut p = RingPresenter::new(None);
        p.submit(3, None, &[]);
        assert!(p.has_fresh_frame());

        let first = p.acquire();
        assert_eq!(first.kind, Present::Fresh);
        assert_eq!(first.key, 3);
        assert_ne!(first.token, 0);
        assert!(!p.has_fresh_frame(), "the pending frame was taken");

        // Nothing new arrived, so the same buffer stays up.
        let second = p.acquire();
        assert_eq!(second.kind, Present::Hold);
        assert_eq!(second.key, 3);
        assert_eq!(
            second.token, first.token,
            "a hold re-presents under the live token, or the buffer on screen \
             would look superseded and be released out from under the display"
        );
    }

    /// Holding forever never accumulates outstanding tokens.
    ///
    /// A layer that is idle for a minute at 60Hz holds 3600 times. One
    /// outstanding entry per hold would be a slow leak with no upper bound.
    #[test]
    fn idle_holds_do_not_accumulate() {
        let mut p = RingPresenter::new(None);
        p.submit(0, None, &[]);
        let token = p.acquire().token;
        for _ in 0..100 {
            assert_eq!(p.acquire().kind, Present::Hold);
        }
        assert_eq!(p.outstanding.len(), 1, "one live token, however many holds");
        assert_eq!(p.release(token), None, "still on screen");
    }

    /// A buffer is released only once something newer replaces it.
    #[test]
    fn a_buffer_is_released_when_superseded() {
        let mut p = RingPresenter::new(None);
        p.submit(0, None, &[]);
        let first = p.acquire().token;
        assert_eq!(
            p.release(first),
            None,
            "releasing the buffer on screen would race the producer into \
             overwriting live scanout"
        );

        p.submit(1, None, &[]);
        let second = p.acquire().token;
        assert_ne!(second, first);
        assert_eq!(p.release(first), Some(0), "slot 0 is now off screen");
        assert_eq!(p.release(first), None, "and is not released twice");
        assert_eq!(p.release(second), None, "the new one is still up");
    }

    /// The sentinel token releases nothing.
    #[test]
    fn the_sentinel_token_releases_nothing() {
        let mut p = RingPresenter::new(None);
        assert_eq!(p.release(0), None);
    }

    /// A buffer stays referenced while on screen or awaiting release.
    #[test]
    fn a_buffer_is_referenced_until_retired() {
        let mut p = RingPresenter::new(None);
        p.submit(5, None, &[]);
        let first = p.acquire().token;
        assert!(p.is_referenced(5));

        p.submit(6, None, &[]);
        p.acquire();
        assert!(p.is_referenced(5), "off screen but not yet retired");
        assert!(p.is_referenced(6));

        assert_eq!(p.release(first), Some(5));
        assert!(!p.is_referenced(5), "retired, so a pool may now evict it");
    }

    /// Submitting twice before a vblank presents the newer frame.
    ///
    /// A producer outrunning the display should show its newest frame, not work
    /// through a backlog it will never catch up on.
    #[test]
    fn a_second_submit_replaces_an_unacquired_first() {
        let mut p = RingPresenter::new(None);
        p.submit(1, None, &[]);
        p.submit(2, None, &[]);
        let out = p.acquire();
        assert_eq!(out.key, 2);
        assert_eq!(p.acquire().kind, Present::Hold, "only one frame was queued");
    }

    /// Damage replaces rather than unions.
    #[test]
    fn damage_replaces_across_submits() {
        let mut p = RingPresenter::new(None);
        p.submit(0, None, &[rect(0, 0), rect(8, 0)]);
        p.submit(0, None, &[rect(16, 0)]);
        let out = p.acquire();
        assert_eq!(out.damage, vec![rect(16, 0)]);
    }

    /// Too much damage degrades to whole-frame.
    ///
    /// Truncating would under-report the dirty area, and the rectangles left
    /// out would keep whatever the previous frame had — visible as stale
    /// patches. Whole-frame is slower and correct.
    #[test]
    fn over_cap_damage_becomes_whole_frame() {
        let mut p = RingPresenter::new(None);
        let many: Vec<DamageRect> = (0..=MAX_DAMAGE)
            .map(|i| rect(i32::try_from(i).expect("small") * 8, 0))
            .collect();
        p.submit(0, None, &many);
        let out = p.acquire();
        assert!(
            out.damage.is_empty(),
            "an over-cap list must degrade to whole-frame, not truncate to \
             {MAX_DAMAGE}"
        );
    }

    /// A reset drops everything, including what is on screen.
    #[test]
    fn a_reset_forgets_the_scanning_buffer() {
        let mut p = RingPresenter::new(None);
        p.submit(4, None, &[]);
        p.acquire();
        assert_eq!(p.scanning_key(), Some(4));

        p.reset();

        assert_eq!(p.scanning_key(), None);
        assert!(!p.is_referenced(4));
        assert_eq!(
            p.acquire().kind,
            Present::None,
            "after a resume nothing is on screen to hold, so there is nothing \
             to present until the producer submits again"
        );
    }

    /// Tokens skip the sentinel when the counter wraps.
    ///
    /// Reaching zero would hand out a token `release` ignores, and that
    /// buffer would never be handed back — the producer's ring would starve one
    /// slot short, permanently.
    #[test]
    fn a_wrapping_token_skips_the_sentinel() {
        let mut p = RingPresenter::new(None);
        p.next_token = u64::MAX;
        p.submit(0, None, &[]);
        let out = p.acquire();
        assert_ne!(out.token, 0, "the sentinel must be skipped on wrap");
        assert_eq!(out.token, 1);
    }
}
