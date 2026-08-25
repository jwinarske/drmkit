// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Print what the seat's devices are doing, with auto-repeat wired in.
//!
//! ```text
//! cargo run -p drmkit-input --example input
//! ```
//!
//! Needs read access to `/dev/input/event*`: the `input` group, or root.
//! Without it libinput enumerates the seat and opens nothing, which looks
//! like a keyboard that does not work -- so run it with
//! `DRMKIT_LOG_LEVEL=debug` to hear libinput say why.
//!
//! Escape stops it. So does thirty seconds of nothing happening.

use std::os::fd::AsFd as _;
use std::time::{Duration, Instant};

use drmkit_input::{
    InputEvent, KeyEvent, KeyRepeater, Keyboard, KeymapNames, RepeatConfig, Seat, SeatOptions,
};
use rustix::event::{PollFd, PollFlags, poll};

/// evdev `KEY_ESC`.
const KEY_ESC: u32 = 1;

fn main() {
    let mut seat = match Seat::open(&SeatOptions::default()) {
        Ok(seat) => seat,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let mut keyboard = Keyboard::from_names(&KeymapNames::default()).expect("keymap");
    let mut repeater = KeyRepeater::new(RepeatConfig::default()).expect("timer");

    let started = Instant::now();
    let mut events = Vec::new();
    let mut repeats: Vec<KeyEvent> = Vec::new();
    let mut leds = keyboard.leds();

    loop {
        {
            let seat_fd = seat.as_fd();
            let timer_fd = repeater.fd();
            let mut fds = [
                PollFd::new(&seat_fd, PollFlags::IN),
                PollFd::new(&timer_fd, PollFlags::IN),
            ];
            let timeout = Duration::from_secs(30).try_into().expect("timeout");
            if poll(&mut fds, Some(&timeout)).expect("poll") == 0 {
                println!("nothing for thirty seconds");
                return;
            }
        }

        let now = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

        events.clear();
        seat.dispatch(&mut events).expect("dispatch");
        for event in &mut events {
            match event {
                InputEvent::Key(key) => {
                    keyboard.process_key(key);
                    repeater.on_key(key, keyboard.should_repeat(key.key));
                    if report(key) {
                        return;
                    }
                }
                other => println!("{other:?}"),
            }
        }

        repeats.clear();
        repeater.dispatch(now, &mut repeats).expect("repeat");
        for key in &mut repeats {
            // Resolved, not replayed: a Shift pressed since the key went down
            // changes what this types.
            keyboard.resolve(key);
            report(key);
        }

        let current = keyboard.leds();
        if current != leds {
            seat.set_leds(current);
            leds = current;
        }
    }
}

/// Print one key. Returns whether it was Escape coming up.
fn report(key: &KeyEvent) -> bool {
    println!(
        "key {:>3} {}{} sym {:#06x} {:?}",
        key.key,
        if key.pressed { "down" } else { "up  " },
        if key.repeat { " (repeat)" } else { "" },
        key.sym,
        key.utf8
    );
    key.key == KEY_ESC && !key.pressed
}
