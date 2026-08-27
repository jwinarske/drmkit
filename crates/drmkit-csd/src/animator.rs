// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Moving a decoration between states over time.

use std::time::Duration;

use crate::{HoverButton, WindowState};

/// Ease-out cubic, clamped to `0..=1`.
///
/// Fast at the start and slow at the end, which is what a transition to a
/// settled state should feel like: the change is announced immediately and
/// arrives gently. The clamp is not decoration — an animator that overshoots
/// its duration would otherwise extrapolate past the target and the window
/// would visibly bounce.
#[must_use]
pub fn ease_out_cubic(t: f32) -> f32 {
    let clamped = t.clamp(0.0, 1.0);
    let inverse = 1.0 - clamped;
    1.0 - (inverse * inverse * inverse)
}

/// One value moving toward a target.
#[derive(Debug, Clone, Copy)]
struct Track {
    /// Where the current run started, so a retarget mid-flight eases from
    /// where the value *is* rather than snapping back to zero.
    start: f32,
    /// Where it is now.
    progress: f32,
    /// How far into the run.
    elapsed: Duration,
    /// Whether a run is in progress.
    running: bool,
}

impl Track {
    const fn settled(at: f32) -> Self {
        Self {
            start: at,
            progress: at,
            elapsed: Duration::ZERO,
            running: false,
        }
    }

    /// Begin moving toward a new target from wherever the value is now.
    const fn retarget(&mut self, from: f32) {
        self.start = from;
        self.elapsed = Duration::ZERO;
        self.running = true;
    }

    /// Advance, and report whether the run is still going.
    fn advance(&mut self, dt: Duration, duration: Duration, target: f32) {
        if !self.running {
            return;
        }
        self.elapsed = self.elapsed.saturating_add(dt);
        // The clamp is belt-and-braces: `ease_out_cubic` clamps too, and the
        // `t >= 1.0` settle below catches an overshoot regardless. Removing it
        // changes no observable behaviour -- verified by injection -- and it
        // stays because the value is compared as well as eased, and a reader
        // should not have to check both to know `t` is in range.
        let t = (self.elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
        self.progress = self.start + (target - self.start) * ease_out_cubic(t);
        if t >= 1.0 {
            // Assigned rather than left to the ease, which reaches 1.0 only in
            // the limit -- a window that settled at 0.999 focused would never
            // quite look focused.
            self.progress = target;
            self.running = false;
        }
    }
}

/// The animations one decorated window is running.
///
/// Focus and hover advance independently: a window can gain focus while the
/// pointer leaves a button, and forcing them onto one timeline would make
/// whichever started second jump.
#[derive(Debug, Clone, Copy)]
pub struct WindowAnim {
    focus_target: bool,
    focus: Track,
    hover_target: HoverButton,
    /// Which button is being *drawn* highlighted.
    ///
    /// Not the same as the target. When the pointer leaves, the target is
    /// `None` immediately but the button it left keeps being drawn while it
    /// fades — without this, the highlight would vanish rather than fade.
    hover_painted: HoverButton,
    hover: Track,
}

impl Default for WindowAnim {
    fn default() -> Self {
        Self::new(false)
    }
}

impl WindowAnim {
    /// An animator settled at `focused`, with nothing hovered.
    ///
    /// Settled, not animating toward it: a window that appears focused should
    /// be drawn focused, not fade in from unfocused on its first frame.
    #[must_use]
    pub const fn new(focused: bool) -> Self {
        Self {
            focus_target: focused,
            focus: Track::settled(if focused { 1.0 } else { 0.0 }),
            hover_target: HoverButton::None,
            hover_painted: HoverButton::None,
            hover: Track::settled(0.0),
        }
    }

    /// Aim at a new focus state.
    ///
    /// Retargeting to what it is already aiming at does nothing — otherwise a
    /// caller that reports focus every frame would restart the timeline every
    /// frame and the window would never finish transitioning.
    pub const fn retarget_focus(&mut self, focused: bool) {
        if self.focus_target == focused {
            return;
        }
        self.focus_target = focused;
        let from = self.focus.progress;
        self.focus.retarget(from);
    }

    /// Aim at a new hovered button.
    ///
    /// Moving between buttons restarts from zero on the new one rather than
    /// carrying the old one's progress across: they are different highlights
    /// in different places, and a value carried over would make the new button
    /// appear already half-lit.
    pub const fn retarget_hover(&mut self, hover: HoverButton) {
        if self.hover_target as u8 == hover as u8 {
            return;
        }
        self.hover_target = hover;
        let from = if matches!(hover, HoverButton::None) {
            // Leaving: keep drawing the button being left, and fade it from
            // where it is.
            self.hover.progress
        } else {
            self.hover_painted = hover;
            self.hover.progress = 0.0;
            0.0
        };
        self.hover.retarget(from);
    }

    /// Jump to the targets, running nothing.
    ///
    /// For a caller that wants the end state now — a window appearing, a
    /// theme with animations turned off, a session resuming.
    pub const fn snap(&mut self) {
        self.focus = Track::settled(if self.focus_target { 1.0 } else { 0.0 });
        self.hover_painted = self.hover_target;
        self.hover = Track::settled(if matches!(self.hover_target, HoverButton::None) {
            0.0
        } else {
            1.0
        });
    }

    /// Advance by `dt`, over a transition lasting `duration`.
    ///
    /// Returns whether anything is still animating, which is what tells a
    /// caller to schedule another frame.
    ///
    /// A zero or negative duration snaps. That is not an edge case to tolerate
    /// but the documented way to turn animations off — `glass-minimal` sets
    /// it — and dividing by it instead would produce an infinity that the
    /// clamp turns into an instant jump anyway, less legibly.
    pub fn tick(&mut self, dt: Duration, duration: Duration) -> bool {
        if duration.is_zero() {
            self.snap();
            return false;
        }

        self.focus
            .advance(dt, duration, if self.focus_target { 1.0 } else { 0.0 });

        // The hover target is 1.0 only while the painted button is still the
        // one being aimed at. Once the pointer has left, the same track runs
        // back down to zero on the button it is still drawing.
        let hover_target = if !matches!(self.hover_target, HoverButton::None)
            && self.hover_target as u8 == self.hover_painted as u8
        {
            1.0
        } else {
            0.0
        };
        let was_running = self.hover.running;
        self.hover.advance(dt, duration, hover_target);
        if was_running && !self.hover.running && hover_target == 0.0 {
            // The fade finished: stop drawing the button that was left.
            self.hover_painted = HoverButton::None;
        }

        self.is_animating()
    }

    /// Whether anything is still moving.
    #[must_use]
    pub const fn is_animating(&self) -> bool {
        self.focus.running || self.hover.running
    }

    /// How far through the focus transition, `0..=1`.
    #[must_use]
    pub const fn focus_progress(&self) -> f32 {
        self.focus.progress
    }

    /// How far through the hover transition, `0..=1`.
    #[must_use]
    pub const fn hover_progress(&self) -> f32 {
        self.hover.progress
    }

    /// Which button is being drawn highlighted.
    #[must_use]
    pub const fn hover_painted(&self) -> HoverButton {
        self.hover_painted
    }

    /// Write the current progress into a state for the renderer.
    pub const fn apply_to(&self, state: &mut WindowState) {
        state.focus_progress = self.focus.progress;
        state.hover = self.hover_painted;
        state.hover_progress = self.hover.progress;
    }
}
