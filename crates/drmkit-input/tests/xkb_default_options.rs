// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Keymap names with no options take xkb's default options, as the
//! reference's do.
//!
//! libxkbcommon fills an empty rules, model, layout or variant from its
//! defaults, but options only when they are absent: an empty string means
//! "no options", and the `XKB_DEFAULT_OPTIONS` a user set is dropped. The
//! reference passes absent options for an empty string. With
//! `ctrl:nocaps` in the environment, Caps Lock is a Control key, or the
//! user's setting was lost.
//!
//! Its own binary, without the test harness: setting the variable is only
//! sound while nothing else is running, and the harness runs cases on threads.

use drmkit_input::{KeyEvent, Keyboard, KeymapNames};

/// `KEY_CAPSLOCK` from `linux/input-event-codes.h`.
const KEY_CAPSLOCK: u32 = 58;

fn main() {
    // SAFETY: this binary is single-threaded -- `harness = false` -- and
    // nothing has started a thread yet.
    unsafe { std::env::set_var("XKB_DEFAULT_OPTIONS", "ctrl:nocaps") };

    let mut keyboard = Keyboard::from_names(&KeymapNames {
        layout: "us".to_owned(),
        ..KeymapNames::default()
    })
    .expect("compile us");
    let mut event = KeyEvent {
        key: KEY_CAPSLOCK,
        pressed: true,
        ..KeyEvent::default()
    };
    keyboard.process_key(&mut event);

    assert!(
        keyboard.modifiers().ctrl,
        "XKB_DEFAULT_OPTIONS=ctrl:nocaps makes Caps Lock a Control key"
    );
    println!("xkb_default_options: Caps Lock is Control under ctrl:nocaps");
}
