// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Deciding how many ioctls one frame across several outputs costs.

/// How to split a frame across the outputs it touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NarrowPolicy {
    /// Never split: one atomic commit for every engaged scene.
    ///
    /// The only policy that makes a multi-output frame **atomic** — either
    /// every output shows the new frame or none does. What it costs is that
    /// one output's refusal takes the others down with it, and that a modeset
    /// anywhere forces `ALLOW_MODESET` on the whole commit.
    Combined,

    /// Split only when the outputs disagree about modesetting.
    ///
    /// The default, and the reason the policy exists. A modeset is expensive
    /// and can blank an output; a steady frame is neither. Committing them
    /// together makes every steady output pay the modeset's cost, so when the
    /// set is mixed they go separately — **modeset first**, since a steady
    /// frame on an output that is about to be reconfigured is wasted work.
    /// When every engaged scene agrees, one commit, and the atomicity is kept.
    #[default]
    AutoOnModeset,

    /// One commit per engaged scene, always.
    ///
    /// Gives up cross-output atomicity to contain failure: an output that
    /// refuses its frame does not stop the others showing theirs. For a
    /// caller that would rather have three screens right and one wrong than
    /// four screens stale.
    PerCrtc,
}

/// What one slot in the set looks like this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlotState {
    /// Whether this slot holds no scene.
    ///
    /// A set keeps its indices stable across removal, so a removed scene
    /// leaves a hole rather than shifting everything after it — a caller's
    /// stored index would otherwise silently start naming a different output.
    pub is_hole: bool,
    /// Whether this scene needs `ALLOW_MODESET` this frame.
    pub wants_modeset: bool,
}

/// Group the engaged slots into one commit each.
///
/// Holes are never in a group: there is nothing to commit for them, and an
/// empty group would be an ioctl that programs nothing.
#[must_use]
pub fn partition_for_policy(slots: &[SlotState], policy: NarrowPolicy) -> Vec<Vec<usize>> {
    let engaged = || {
        slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| !slot.is_hole)
            .map(|(index, _)| index)
    };

    match policy {
        NarrowPolicy::PerCrtc => engaged().map(|index| vec![index]).collect(),

        NarrowPolicy::Combined => {
            let combined: Vec<usize> = engaged().collect();
            if combined.is_empty() {
                Vec::new()
            } else {
                vec![combined]
            }
        }

        NarrowPolicy::AutoOnModeset => {
            let (modeset, steady): (Vec<usize>, Vec<usize>) =
                engaged().partition(|index| slots[*index].wants_modeset);

            match (modeset.is_empty(), steady.is_empty()) {
                // Nothing engaged: no commits at all.
                (true, true) => Vec::new(),
                // Mixed: modeset first, so a steady frame is not committed to
                // an output that is about to be reconfigured out from under it.
                (false, false) => vec![modeset, steady],
                // Uniform either way: one commit, and the set stays atomic.
                (false, true) => vec![modeset],
                (true, false) => vec![steady],
            }
        }
    }
}
