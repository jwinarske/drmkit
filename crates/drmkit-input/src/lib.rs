// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Keyboard, pointer and touch input.
//!
//! Two halves that are useful apart. [`Keyboard`] compiles an xkb keymap and
//! turns evdev keycodes into keysyms and text; the libinput side reads the
//! devices those codes come from. A caller with its own event source -- a
//! remote protocol, a test harness, a replay -- wants the first without the
//! second, so they are separate and each is behind its own feature.
//!
//! # Features
//!
//! `xkb` (default) brings in [`Keyboard`]. Without it the event types and the
//! parts of this crate that are plain state machines still build, which is
//! what a cross target whose sysroot has no libxkbcommon can do.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod event;
mod repeat;
mod repeater;

#[cfg(test)]
mod repeat_tests;

#[cfg(test)]
mod repeater_tests;

#[cfg(feature = "xkb")]
mod keyboard;

#[cfg(all(test, feature = "xkb"))]
mod keyboard_tests;

pub use repeat::{Repeat, RepeatConfig, Timer};
pub use repeater::KeyRepeater;

pub use event::{
    InputEvent, KeyEvent, PointerAxis, PointerButton, PointerEvent, PointerMotion, Switch,
    SwitchEvent, TouchEvent, TouchPhase,
};

#[cfg(feature = "xkb")]
pub use keyboard::{Keyboard, KeymapNames, Leds, Modifiers};

/// What can go wrong reading input.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InputError {
    /// The keymap did not compile.
    ///
    /// A layout that is not installed, a variant that does not exist, an
    /// option xkb does not know. xkb reports the detail through its own log,
    /// not through a return value, so there is nothing more to pass on here.
    #[error("the keymap did not compile")]
    Keymap,

    /// The configuration cannot work.
    #[error("{0}")]
    Config(&'static str),

    /// A timer could not be created or read.
    #[error("repeat timer")]
    Timer(#[source] std::io::Error),

    /// A keymap file could not be read.
    #[error("reading {path}")]
    Io {
        /// The file that could not be read.
        path: std::path::PathBuf,
        /// Why.
        source: std::io::Error,
    },
}
