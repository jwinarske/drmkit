// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What auto-repeat does, with no timer involved.

use crate::event::KeyEvent;
use crate::repeat::{Repeat, RepeatConfig, Timer};

const KEY_A: u32 = 30;
const KEY_B: u32 = 48;
const KEY_LEFTSHIFT: u32 = 42;

fn press(key: u32) -> KeyEvent {
    KeyEvent {
        key,
        pressed: true,
        ..KeyEvent::default()
    }
}

fn release(key: u32) -> KeyEvent {
    KeyEvent {
        key,
        pressed: false,
        ..KeyEvent::default()
    }
}

fn repeat() -> Repeat {
    Repeat::new(RepeatConfig::default())
}

#[test]
fn a_held_letter_starts_repeating() {
    let mut repeat = repeat();
    assert_eq!(repeat.on_key(&press(KEY_A), true), Timer::Arm);
    assert_eq!(repeat.held_key(), Some(KEY_A));
}

#[test]
fn releasing_it_stops() {
    let mut repeat = repeat();
    repeat.on_key(&press(KEY_A), true);
    assert_eq!(repeat.on_key(&release(KEY_A), true), Timer::Disarm);
    assert_eq!(repeat.held_key(), None);
}

#[test]
fn a_modifier_does_not_disturb_a_key_already_repeating() {
    // Holding `a` and reaching for Shift should go on repeating -- in upper
    // case from the next tick, which is what re-resolving buys.
    let mut repeat = repeat();
    repeat.on_key(&press(KEY_A), true);

    assert_eq!(
        repeat.on_key(&press(KEY_LEFTSHIFT), false),
        Timer::Unchanged
    );
    assert_eq!(repeat.held_key(), Some(KEY_A));

    assert_eq!(
        repeat.on_key(&release(KEY_LEFTSHIFT), false),
        Timer::Unchanged,
        "and letting go of it does not stop the letter either"
    );
    assert_eq!(repeat.held_key(), Some(KEY_A));
}

#[test]
fn a_second_letter_takes_over_from_the_first() {
    // Rolling from one key to the next without releasing: the newest one is
    // what repeats, and it starts from the delay rather than mid-interval.
    let mut repeat = repeat();
    repeat.on_key(&press(KEY_A), true);
    assert_eq!(repeat.on_key(&press(KEY_B), true), Timer::Arm);
    assert_eq!(repeat.held_key(), Some(KEY_B));
}

#[test]
fn releasing_the_key_that_was_taken_over_from_changes_nothing() {
    let mut repeat = repeat();
    repeat.on_key(&press(KEY_A), true);
    repeat.on_key(&press(KEY_B), true);

    assert_eq!(repeat.on_key(&release(KEY_A), true), Timer::Unchanged);
    assert_eq!(repeat.held_key(), Some(KEY_B), "b is still repeating");
}

#[test]
fn a_synthesized_event_is_not_fed_back_in() {
    // The output of `synthesize` looks exactly like a press. Acting on it
    // would re-arm the delay every tick and the key would never repeat.
    let mut repeat = repeat();
    repeat.on_key(&press(KEY_A), true);

    let mut synthesized = Vec::new();
    repeat.synthesize(1, 0, &mut synthesized);

    assert_eq!(repeat.on_key(&synthesized[0], true), Timer::Unchanged);
    assert_eq!(repeat.held_key(), Some(KEY_A));
}

#[test]
fn cancelling_stops_whatever_was_running() {
    let mut repeat = repeat();
    repeat.on_key(&press(KEY_A), true);
    assert_eq!(repeat.cancel(), Timer::Disarm);
    assert_eq!(repeat.held_key(), None);
}

#[test]
fn nothing_held_synthesizes_nothing() {
    // The timer can still be readable after a cancel: the expirations it
    // already collected do not go away.
    let repeat = repeat();
    let mut out = Vec::new();
    assert_eq!(repeat.synthesize(4, 0, &mut out), 0);
    assert!(out.is_empty());
}

#[test]
fn a_synthesized_event_says_what_it_is() {
    let mut repeat = repeat();
    repeat.on_key(&press(KEY_A), true);

    let mut out = Vec::new();
    assert_eq!(repeat.synthesize(1, 1234, &mut out), 0);
    assert_eq!(
        out,
        vec![KeyEvent {
            time_ms: 1234,
            key: KEY_A,
            pressed: true,
            repeat: true,
            sym: 0,
            utf8: String::new(),
        }]
    );
}

#[test]
fn a_backlog_is_capped_and_the_drop_is_reported() {
    // A loop that stalled for two seconds comes back to fifty expirations.
    // Emitting them all types fifty characters the user did not.
    let mut repeat = repeat();
    repeat.on_key(&press(KEY_A), true);

    let mut out = Vec::new();
    assert_eq!(repeat.synthesize(50, 0, &mut out), 42, "and says so");
    assert_eq!(out.len(), 8);
}

#[test]
fn a_key_the_keymap_does_not_repeat_never_starts() {
    let mut repeat = repeat();
    assert_eq!(
        repeat.on_key(&press(KEY_LEFTSHIFT), false),
        Timer::Unchanged
    );
    assert_eq!(repeat.held_key(), None);

    let mut out = Vec::new();
    repeat.synthesize(3, 0, &mut out);
    assert!(out.is_empty());
}
