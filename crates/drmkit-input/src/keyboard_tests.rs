// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The xkb side, against keymaps compiled here rather than the system's.

use crate::event::KeyEvent;
use crate::keyboard::{Keyboard, KeymapNames};
use crate::leds::Leds;

/// Evdev codes used below.
const KEY_A: u32 = 30;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_CAPSLOCK: u32 = 58;
const KEY_APOSTROPHE: u32 = 40;

/// The same keymap with an ordinary Shift, for everything the latch is not
/// the point of.
const PLAIN_KEYMAP: &str = r#"
xkb_keymap {
  xkb_keycodes {
    minimum = 8;
    maximum = 255;
    <LFSH> = 50;
    <AC01> = 38;
    <CAPS> = 66;
  };
  xkb_types { include "basic" };
  xkb_compat {
    interpret Shift_L { action = SetMods(modifiers = Shift); };
    interpret Caps_Lock { action = LockMods(modifiers = Lock); };
    indicator "Caps Lock" { modifiers = Lock; };
  };
  xkb_symbols {
    key <LFSH> { [ Shift_L ] };
    key <CAPS> { [ Caps_Lock ] };
    key <AC01> { type = "ALPHABETIC", [ a, A ] };
    modifier_map Shift { <LFSH> };
    modifier_map Lock { <CAPS> };
  };
};
"#;

/// A keymap with a *latching* Shift, which is what makes the resolve/update
/// order observable: pressing a letter is the event that spends the latch.
const LATCH_KEYMAP: &str = r#"
xkb_keymap {
  xkb_keycodes {
    minimum = 8;
    maximum = 255;
    <LFSH> = 50;
    <AC01> = 38;
    <CAPS> = 66;
  };
  xkb_types { include "basic" };
  xkb_compat {
    interpret Shift_L { action = LatchMods(modifiers = Shift); };
    interpret Caps_Lock { action = LockMods(modifiers = Lock); };
    indicator "Caps Lock" { modifiers = Lock; };
  };
  xkb_symbols {
    key <LFSH> { [ Shift_L ] };
    key <CAPS> { [ Caps_Lock ] };
    key <AC01> { type = "ALPHABETIC", [ a, A ] };
    modifier_map Shift { <LFSH> };
    modifier_map Lock { <CAPS> };
  };
};
"#;

fn press(keyboard: &mut Keyboard, key: u32) -> KeyEvent {
    let mut event = KeyEvent {
        key,
        pressed: true,
        ..KeyEvent::default()
    };
    keyboard.process_key(&mut event);
    event
}

fn release(keyboard: &mut Keyboard, key: u32) -> KeyEvent {
    let mut event = KeyEvent {
        key,
        pressed: false,
        ..KeyEvent::default()
    };
    keyboard.process_key(&mut event);
    event
}

#[test]
fn a_latched_shift_still_applies_to_the_key_that_spends_it() {
    // The whole reason resolution comes before the state update. Pressing `a`
    // is what consumes the latch, so a keyboard that updates first reads a
    // state the user has already left and types a lowercase letter.
    let mut keyboard = Keyboard::from_string(LATCH_KEYMAP).expect("compile");

    press(&mut keyboard, KEY_LEFTSHIFT);
    release(&mut keyboard, KEY_LEFTSHIFT);
    assert!(keyboard.modifiers().shift, "the latch is armed");

    let typed = press(&mut keyboard, KEY_A);
    assert_eq!(typed.utf8, "A");
    assert!(!keyboard.modifiers().shift, "and the latch is spent");
}

#[test]
fn a_held_shift_applies_to_the_letter_after_it() {
    let mut keyboard = Keyboard::from_string(PLAIN_KEYMAP).expect("compile");
    assert_eq!(press(&mut keyboard, KEY_A).utf8, "a");

    press(&mut keyboard, KEY_LEFTSHIFT);
    assert_eq!(press(&mut keyboard, KEY_A).utf8, "A");
}

#[test]
fn held_keys_are_tracked_and_released() {
    let mut keyboard = Keyboard::from_string(PLAIN_KEYMAP).expect("compile");
    press(&mut keyboard, KEY_LEFTSHIFT);
    press(&mut keyboard, KEY_A);
    assert_eq!(keyboard.held_keys(), [KEY_LEFTSHIFT, KEY_A]);

    release(&mut keyboard, KEY_LEFTSHIFT);
    assert_eq!(keyboard.held_keys(), [KEY_A]);
}

#[test]
fn a_repeated_press_is_not_tracked_twice() {
    // libinput does not send one, but a resume that re-adds a device can.
    let mut keyboard = Keyboard::from_string(PLAIN_KEYMAP).expect("compile");
    press(&mut keyboard, KEY_A);
    press(&mut keyboard, KEY_A);
    assert_eq!(keyboard.held_keys(), [KEY_A]);

    release(&mut keyboard, KEY_A);
    assert!(keyboard.held_keys().is_empty(), "and one release clears it");
}

#[test]
fn releasing_everything_drops_the_modifiers_but_not_the_locks() {
    // The VT-switch case: Ctrl and Alt were let go while the device was
    // revoked, so their releases never arrive. Caps Lock is not something the
    // user let go of.
    let mut keyboard = Keyboard::from_string(PLAIN_KEYMAP).expect("compile");
    press(&mut keyboard, KEY_CAPSLOCK);
    release(&mut keyboard, KEY_CAPSLOCK);
    press(&mut keyboard, KEY_LEFTSHIFT);
    assert!(keyboard.modifiers().shift);

    keyboard.release_all();

    assert!(keyboard.held_keys().is_empty());
    assert!(!keyboard.modifiers().shift, "nothing is held any more");
    assert!(keyboard.leds().caps_lock, "and Caps Lock is still on");
}

#[test]
fn the_lock_state_can_be_driven_from_outside() {
    let mut keyboard = Keyboard::from_string(PLAIN_KEYMAP).expect("compile");
    assert!(!keyboard.leds().caps_lock);

    keyboard.set_leds(Leds {
        caps_lock: true,
        ..Leds::default()
    });
    assert!(keyboard.leds().caps_lock);

    // Idempotent: asking for what is already true taps nothing.
    keyboard.set_leds(Leds {
        caps_lock: true,
        ..Leds::default()
    });
    assert!(keyboard.leds().caps_lock);
}

#[test]
fn a_keymap_that_does_not_compile_leaves_nothing_behind() {
    assert!(Keyboard::from_string("this is not a keymap").is_err());
}

#[test]
fn modifiers_and_letters_differ_on_whether_they_repeat() {
    let keyboard = Keyboard::from_names(&KeymapNames::default()).expect("system default keymap");
    assert!(keyboard.should_repeat(KEY_A), "letters repeat");
    assert!(!keyboard.should_repeat(KEY_LEFTSHIFT), "modifiers do not");
    assert!(!keyboard.should_repeat(KEY_CAPSLOCK), "nor do locks");
}

#[test]
fn a_reload_keeps_what_is_held_and_what_is_locked() {
    let mut keyboard = Keyboard::from_names(&KeymapNames {
        layout: "us".to_owned(),
        ..KeymapNames::default()
    })
    .expect("us");
    press(&mut keyboard, KEY_CAPSLOCK);
    release(&mut keyboard, KEY_CAPSLOCK);
    press(&mut keyboard, KEY_LEFTSHIFT);

    keyboard
        .reload(&KeymapNames {
            layout: "de".to_owned(),
            ..KeymapNames::default()
        })
        .expect("de");

    assert_eq!(keyboard.held_keys(), [KEY_LEFTSHIFT]);
    assert!(keyboard.modifiers().shift, "still held across the swap");
    assert!(keyboard.leds().caps_lock, "and still locked");
}

#[test]
fn a_reload_that_does_not_compile_leaves_the_keymap_alone() {
    let mut keyboard = Keyboard::from_names(&KeymapNames {
        layout: "us".to_owned(),
        ..KeymapNames::default()
    })
    .expect("us");
    press(&mut keyboard, KEY_A);

    assert!(
        keyboard
            .reload(&KeymapNames {
                layout: "there-is-no-such-layout".to_owned(),
                ..KeymapNames::default()
            })
            .is_err()
    );

    assert_eq!(press(&mut keyboard, KEY_A).utf8, "a", "still usable");
}

#[test]
fn resolving_does_not_move_the_state() {
    // What a repeat does: the same key, read against the modifiers held now,
    // without pretending the user pressed it again.
    let mut keyboard = Keyboard::from_string(LATCH_KEYMAP).expect("compile");
    press(&mut keyboard, KEY_LEFTSHIFT);

    let mut event = KeyEvent {
        key: KEY_A,
        pressed: true,
        repeat: true,
        ..KeyEvent::default()
    };
    keyboard.resolve(&mut event);
    assert_eq!(event.utf8, "A");
    assert_eq!(keyboard.held_keys(), [KEY_LEFTSHIFT], "and `a` is not held");
    assert!(keyboard.modifiers().shift, "nor was the latch spent");
}

#[test]
fn a_latching_layout_types_what_the_user_latched() {
    // The same ordering, on a layout people have installed rather than one
    // written for the test. Latvian writes its long vowels by latching level
    // three with the apostrophe key: ' then a is how a-macron is typed.
    let mut keyboard = Keyboard::from_names(&KeymapNames {
        layout: "lv".to_owned(),
        variant: "apostrophe".to_owned(),
        ..KeymapNames::default()
    })
    .expect("lv(apostrophe)");

    press(&mut keyboard, KEY_APOSTROPHE);
    release(&mut keyboard, KEY_APOSTROPHE);
    assert_eq!(press(&mut keyboard, KEY_A).utf8, "ā");
}

#[test]
fn reaching_for_shift_changes_what_the_next_repeat_types() {
    // The two halves together, and the reason repeats resolve rather than
    // replay: the key does not change, the modifiers under it do.
    use crate::repeat::{Repeat, RepeatConfig};

    let mut keyboard = Keyboard::from_string(PLAIN_KEYMAP).expect("compile");
    let mut repeat = Repeat::new(RepeatConfig::default());

    let held = press(&mut keyboard, KEY_A);
    repeat.on_key(&held, keyboard.should_repeat(held.key));

    let mut ticks = Vec::new();
    repeat.synthesize(1, 0, &mut ticks);
    keyboard.resolve(&mut ticks[0]);
    assert_eq!(ticks[0].utf8, "a");

    let shift = press(&mut keyboard, KEY_LEFTSHIFT);
    repeat.on_key(&shift, keyboard.should_repeat(shift.key));
    assert_eq!(repeat.held_key(), Some(KEY_A), "still the letter repeating");

    ticks.clear();
    repeat.synthesize(1, 0, &mut ticks);
    keyboard.resolve(&mut ticks[0]);
    assert_eq!(ticks[0].utf8, "A");
}
