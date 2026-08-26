// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Deciding whether this vblank is worth a commit at all.

/// What the present loop should do with this frame.
///
/// One enum rather than the reference's `{ FrameAction action; bool full; }`,
/// where `full` is meaningless unless `action` is `Commit` and is set to
/// `false` on a skip — a value a caller can read and act on. Three variants
/// make the nonsensical combination unrepresentable, and leave the caller one
/// thing to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAction {
    /// Nothing changed. Issue no commit: no page flip, no scanout reprogram.
    Skip,
    /// Commit the whole frame, omitting `FB_DAMAGE_CLIPS`.
    CommitFull,
    /// Commit with the producer's damage rectangles attached.
    CommitDamaged,
}

impl FrameAction {
    /// Whether this action puts a commit on the wire.
    #[must_use]
    pub const fn commits(self) -> bool {
        !matches!(self, Self::Skip)
    }
}

/// The per-frame present decision, and what it has decided so far.
///
/// # The point of it
///
/// The win is the skip. When nothing has changed since the last commit, the
/// present loop issues no atomic commit at all — no flip, no reprogram. That
/// is power saved on every panel, and on a panel capable of self-refresh it is
/// what lets the panel stay there.
///
/// So the skip is **unconditional** and consults no PSR capability. Not
/// flipping is a win whether or not the panel can self-refresh, and gating it
/// on a capability would mean burning power on the panels that advertise
/// nothing — which is most of them.
///
/// # Threading
///
/// Not synchronised: one present loop owns it, the same one that owns the
/// [`BufferRing`](crate::BufferRing).
#[derive(Debug, Default)]
pub struct FrameEconomy {
    first: bool,
    force_full: bool,
    committed: u64,
    skipped: u64,
}

impl FrameEconomy {
    /// A fresh economy, whose first decision is always a full commit.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            first: true,
            force_full: false,
            committed: 0,
            skipped: 0,
        }
    }

    /// Decide this frame.
    ///
    /// `content_changed` is whether a layer produced a new frame or the scene
    /// mutated since the last committed one. `damage_available` is whether the
    /// producer supplied a bounded damage rectangle set, so a damaged commit is
    /// possible at all.
    ///
    /// The first decision after construction or [`force_full`](Self::force_full)
    /// is always [`FrameAction::CommitFull`]: the scanout buffer's contents are
    /// otherwise undefined, or a modeset has just discarded them.
    pub const fn decide(&mut self, content_changed: bool, damage_available: bool) -> FrameAction {
        if self.first || self.force_full {
            self.first = false;
            self.force_full = false;
            self.committed += 1;
            return FrameAction::CommitFull;
        }
        if !content_changed {
            self.skipped += 1;
            return FrameAction::Skip;
        }
        self.committed += 1;
        if damage_available {
            FrameAction::CommitDamaged
        } else {
            FrameAction::CommitFull
        }
    }

    /// Make the next decision a full commit.
    ///
    /// For after a modeset, a mode change or a rebind — anything that leaves
    /// the previous scanout contents no longer applying, so damage against them
    /// would repaint the wrong difference. One-shot: it applies to the next
    /// decision and then clears.
    pub const fn force_full(&mut self) {
        self.force_full = true;
    }

    /// How many frames have been committed.
    #[must_use]
    pub const fn committed(&self) -> u64 {
        self.committed
    }

    /// How many frames have been skipped — the ones that cost nothing.
    #[must_use]
    pub const fn skipped(&self) -> u64 {
        self.skipped
    }
}
