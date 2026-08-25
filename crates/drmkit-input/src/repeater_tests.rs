// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The repeater against a real timerfd.
//!
//! The waits here are for something that will happen, not for a fixed
//! interval: each polls with a generous timeout and fails if the timer never
//! fires. A slow machine makes them slower, not red.

use std::time::Duration;

use rustix::event::{PollFd, PollFlags, poll};

use crate::InputError;
use crate::event::KeyEvent;
use crate::repeat::RepeatConfig;
use crate::repeater::KeyRepeater;

const KEY_A: u32 = 30;

/// Short enough to keep the suite quick, long enough that a press followed
/// immediately by a release is genuinely a race the repeater has to win.
fn quick() -> RepeatConfig {
    RepeatConfig {
        delay: Duration::from_millis(5),
        interval: Duration::from_millis(5),
    }
}

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

/// Whether the timer became readable within `timeout`.
fn fired(repeater: &KeyRepeater, timeout: Duration) -> bool {
    let fd = repeater.fd();
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    let ready = poll(&mut fds, Some(&timeout.try_into().expect("timeout"))).expect("poll");
    ready > 0
}

#[test]
fn an_interval_of_zero_is_refused() {
    // It would leave the timer firing as fast as the loop can be woken.
    let result = KeyRepeater::new(RepeatConfig {
        delay: Duration::from_millis(100),
        interval: Duration::ZERO,
    });
    assert!(matches!(result, Err(InputError::Config(_))), "{result:?}");
}

#[test]
fn an_idle_repeater_never_fires() {
    let repeater = KeyRepeater::new(quick()).expect("timer");
    assert!(!fired(&repeater, Duration::from_millis(50)));
}

#[test]
fn a_held_key_reaches_the_consumer() {
    let mut repeater = KeyRepeater::new(quick()).expect("timer");
    repeater.on_key(&press(KEY_A), true);
    assert!(fired(&repeater, Duration::from_secs(5)), "the timer fired");

    let mut out = Vec::new();
    repeater.dispatch(7, &mut out).expect("dispatch");
    assert!(!out.is_empty());
    assert!(out.iter().all(|event| event.key == KEY_A && event.repeat));
}

#[test]
fn a_delay_of_zero_still_repeats() {
    // A zero it_value means "disarm" to the kernel, so a repeater that passed
    // the caller's zero straight through would silently never repeat at all.
    let mut repeater = KeyRepeater::new(RepeatConfig {
        delay: Duration::ZERO,
        interval: Duration::from_millis(5),
    })
    .expect("timer");
    repeater.on_key(&press(KEY_A), true);
    assert!(fired(&repeater, Duration::from_secs(5)));

    let mut out = Vec::new();
    repeater.dispatch(0, &mut out).expect("dispatch");
    assert!(!out.is_empty());
}

#[test]
fn a_released_key_stops_the_timer() {
    let mut repeater = KeyRepeater::new(quick()).expect("timer");
    repeater.on_key(&press(KEY_A), true);
    repeater.on_key(&release(KEY_A), true);

    assert!(!fired(&repeater, Duration::from_millis(50)));
    assert_eq!(repeater.held_key(), None);
}

#[test]
fn cancelling_stops_the_timer() {
    let mut repeater = KeyRepeater::new(quick()).expect("timer");
    repeater.on_key(&press(KEY_A), true);
    repeater.cancel();

    assert!(!fired(&repeater, Duration::from_millis(50)));
}

#[test]
fn dispatching_an_unfired_timer_is_quiet() {
    // EAGAIN, which is what a loop sees when it polls several descriptors and
    // this one was not the ready one.
    let mut repeater = KeyRepeater::new(quick()).expect("timer");
    repeater.on_key(&press(KEY_A), true);

    let mut out = Vec::new();
    repeater.dispatch(0, &mut out).expect("dispatch");
    assert!(out.is_empty());
}

#[test]
fn a_cancel_after_the_timer_fired_emits_nothing() {
    // The expirations the kernel already counted do not go away when the
    // repeat is cancelled, and a key the user has let go of should not arrive
    // afterwards.
    let mut repeater = KeyRepeater::new(quick()).expect("timer");
    repeater.on_key(&press(KEY_A), true);
    assert!(fired(&repeater, Duration::from_secs(5)));
    repeater.cancel();

    let mut out = Vec::new();
    repeater.dispatch(0, &mut out).expect("dispatch");
    assert!(out.is_empty());
}
