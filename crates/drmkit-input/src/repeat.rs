// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Deciding what auto-repeat should do, with no timer and no keymap in sight.

use std::time::Duration;

use crate::event::KeyEvent;

/// How many synthesized events one dispatch will produce.
///
/// A loop that stalled comes back to a timerfd holding every expiration it
/// missed. Emitting all of them hands the consumer a burst of keystrokes the
/// user did not type, seconds after they stopped typing; stopping at a
/// handful keeps the key alive without the flood.
const MAX_BURST: u64 = 8;

/// When a held key starts repeating, and how fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeatConfig {
    /// How long a key must be held before the first repeat.
    pub delay: Duration,
    /// The gap between repeats after that.
    pub interval: Duration,
}

impl Default for RepeatConfig {
    /// What X11 and every Wayland compositor settled on: 600 ms, then 25 Hz.
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(600),
            interval: Duration::from_millis(40),
        }
    }
}

/// What the timer should do about a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    /// Start the delay, then repeat at the interval.
    Arm,
    /// Stop.
    Disarm,
    /// Whatever it was doing, keep doing.
    Unchanged,
}

/// Which key is repeating, and what each event does to that.
///
/// Split out from the timer so it can be reasoned about, and tested, without
/// one. libinput deliberately does not auto-repeat -- it reports what the
/// hardware did, and the repeat rate is a user preference the compositor
/// owns -- so this is the piece that has to exist somewhere.
#[derive(Debug, Clone)]
pub struct Repeat {
    config: RepeatConfig,
    held: Option<u32>,
}

impl Repeat {
    /// Start with nothing held.
    #[must_use]
    pub const fn new(config: RepeatConfig) -> Self {
        Self { config, held: None }
    }

    /// The delay and interval this was built with.
    #[must_use]
    pub const fn config(&self) -> RepeatConfig {
        self.config
    }

    /// The key currently repeating.
    #[must_use]
    pub const fn held_key(&self) -> Option<u32> {
        self.held
    }

    /// Feed a real key event.
    ///
    /// `repeats` is the keymap's answer for this key --
    /// [`Keyboard::should_repeat`](crate::Keyboard::should_repeat). It is
    /// passed in rather than looked up so this stays usable without a keymap,
    /// and so a caller with its own policy can impose it.
    ///
    /// Synthesized events are ignored: feeding this its own output back would
    /// re-arm the delay on every tick and the key would never repeat.
    pub fn on_key(&mut self, event: &KeyEvent, repeats: bool) -> Timer {
        if event.repeat {
            return Timer::Unchanged;
        }

        if event.pressed {
            if !repeats {
                // A modifier, or a lock. It does not disturb a key already
                // repeating -- holding `a` and pressing Shift should go on
                // repeating, in upper case from the next tick.
                return Timer::Unchanged;
            }
            self.held = Some(event.key);
            return Timer::Arm;
        }

        // A release only ends the repeat if it is the key that was repeating.
        // Letting go of something else -- a modifier, a key pressed and
        // released while this one is held -- leaves it running.
        if self.held == Some(event.key) {
            self.held = None;
            Timer::Disarm
        } else {
            Timer::Unchanged
        }
    }

    /// Stop repeating.
    ///
    /// For a session pause, or any point where the held-key state has stopped
    /// being trustworthy.
    pub fn cancel(&mut self) -> Timer {
        self.held = None;
        Timer::Disarm
    }

    /// Turn a batch of timer expirations into events to deliver.
    ///
    /// Returns how many were dropped to [`MAX_BURST`], which is worth saying
    /// out loud rather than swallowing: a nonzero answer means the loop is not
    /// keeping up.
    ///
    /// The events carry only the key. A caller that wants `sym` and `utf8`
    /// runs them through [`Keyboard::resolve`](crate::Keyboard::resolve),
    /// which reads against the modifiers held *now* -- so pressing Shift
    /// partway through a held key changes what the next repeat types.
    pub fn synthesize(&self, expirations: u64, time_ms: u32, out: &mut Vec<KeyEvent>) -> u64 {
        let Some(key) = self.held else {
            return 0;
        };
        let emit = expirations.min(MAX_BURST);
        out.reserve(usize::try_from(emit).unwrap_or(usize::MAX));
        for _ in 0..emit {
            out.push(KeyEvent {
                time_ms,
                key,
                pressed: true,
                repeat: true,
                sym: 0,
                utf8: String::new(),
            });
        }
        expirations - emit
    }
}
