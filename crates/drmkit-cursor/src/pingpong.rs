// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Which cursor buffer is on screen, and which one is safe to draw into.
//!
//! A cursor is redrawn while the display is scanning it out — every animation
//! frame, every shape change. Rewriting the buffer the hardware is reading
//! tears for about a vblank between the draw and the commit taking effect.
//! i915 happens to mask that in its cursor pipeline; nothing says it has to.
//!
//! So there are two buffers and this decides which is which. It holds no
//! pixels and no descriptors — just the indices and one flag — which is what
//! makes the rule testable on its own.

/// Which of two cursor buffers is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingPong {
    /// The buffer scanout is reading.
    active: usize,
    /// Whether the other one holds pixels that have not reached the screen.
    drawn: bool,
    /// Whether there is only one buffer after all.
    single: bool,
}

impl PingPong {
    /// A fresh pair, buffer 0 live.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            active: 0,
            drawn: false,
            single: false,
        }
    }

    /// A single buffer, for the legacy path.
    ///
    /// `drmModeSetCursor` uploads the pixels when the cursor is installed and
    /// would need reinstalling for a content change anyway, so a second buffer
    /// buys nothing there.
    #[must_use]
    pub const fn single() -> Self {
        Self {
            active: 0,
            drawn: false,
            single: true,
        }
    }

    /// The buffer scanout is reading.
    #[must_use]
    pub const fn active(self) -> usize {
        self.active
    }

    /// The buffer to draw the next frame into.
    ///
    /// The one that is not on screen, so drawing cannot tear. With a single
    /// buffer there is no choice, and the tear is accepted — the legacy path
    /// rejects rotation and reinstalls on content change, so the window is
    /// narrow.
    #[must_use]
    pub const fn draw_target(self) -> usize {
        if self.single {
            self.active
        } else {
            self.active ^ 1
        }
    }

    /// The buffer the next commit should publish.
    ///
    /// Whatever was just drawn, if anything was. A commit that only moves the
    /// cursor re-publishes the buffer already on screen rather than flipping
    /// to a stale one.
    #[must_use]
    pub const fn commit_target(self) -> usize {
        if self.drawn && !self.single {
            self.active ^ 1
        } else {
            self.active
        }
    }

    /// Note that a frame has been drawn into [`PingPong::draw_target`].
    pub const fn mark_drawn(&mut self) {
        if !self.single {
            self.drawn = true;
        }
    }

    /// Whether a draw is waiting to reach the screen.
    #[must_use]
    pub const fn has_undrawn(self) -> bool {
        self.drawn
    }

    /// Note that a commit published [`PingPong::commit_target`].
    ///
    /// The buffer just published becomes the live one, so the next draw goes
    /// to the one now safely off screen.
    ///
    /// Call this on a commit that *landed*. Where the caller owns the commit
    /// there is no way to know, and calling it optimistically is the right
    /// trade: if their commit failed, scanout is still reading the buffer this
    /// just marked as the draw target, and the next draw there can tear —
    /// which is exactly what a single buffer does all the time. A successful
    /// commit, the overwhelmingly common case, is tear-free.
    pub const fn published(&mut self) {
        if self.drawn {
            self.active ^= 1;
            self.drawn = false;
        }
    }
}

impl Default for PingPong {
    fn default() -> Self {
        Self::new()
    }
}
