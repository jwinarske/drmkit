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
//! `xkb` (default) brings in [`Keyboard`]; `libinput` (default) brings in
//! [`Seat`]. Without either, the event types, [`Repeat`] and [`KeyRepeater`]
//! still build and test -- which is what a cross target with neither library
//! in its sysroot can do.
//!
//! # Not here
//!
//! `SeatOptions::log_handler`, the reference's per-seat sink for libinput's
//! diagnostics. libinput carries one `user_data` slot per context and the
//! `input` crate has already spent it on the device opener, so a per-context
//! sink would need a registry keyed on the context pointer -- for a hook
//! whose stated purpose is separating libinput's stream from the library's
//! own, which [`drmkit_log::set_log_sink`] can already do process-wide.
//! [`SeatOptions::log_priority`] still gates how much libinput says.
//!
//! The `Pointer` accumulator and `EventDispatcher`. The first turns relative
//! motion into a position, which needs a screen extent to clamp against and
//! belongs where that is known; the second is a list of callbacks, which is
//! what a caller writes around a `Vec` of events anyway.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod event;
mod leds;

#[cfg(feature = "libinput")]
mod libinput_log;

#[cfg(feature = "libinput")]
mod seat;

mod repeat;
mod repeater;
#[cfg(all(test, feature = "libinput"))]
mod seat_tests;

#[cfg(test)]
mod repeat_tests;

#[cfg(test)]
mod repeater_tests;

#[cfg(feature = "xkb")]
mod keyboard;

#[cfg(all(test, feature = "xkb"))]
mod keyboard_tests;

pub use leds::Leds;

#[cfg(feature = "libinput")]
pub use input::LibinputInterface;
#[cfg(feature = "libinput")]
pub use libinput_log::LogPriority;
pub use repeat::{Repeat, RepeatConfig, Timer};
pub use repeater::KeyRepeater;
#[cfg(feature = "libinput")]
pub use seat::{Seat, SeatOptions};

pub use event::{
    InputEvent, KeyEvent, PointerAbsolute, PointerAxis, PointerButton, PointerEvent, PointerMotion,
    ScrollSource, Switch, SwitchEvent, TouchEvent, TouchPhase,
};

#[cfg(feature = "xkb")]
pub use keyboard::{Keyboard, KeymapNames, Modifiers};

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

    /// The seat could not be opened.
    #[error("no such seat, or no permission for it: {0}")]
    Seat(String),

    /// libinput could not read its devices.
    #[error("libinput dispatch")]
    Dispatch(#[source] std::io::Error),

    /// A keymap file could not be read.
    #[error("reading {path}")]
    Io {
        /// The file that could not be read.
        path: std::path::PathBuf,
        /// Why.
        source: std::io::Error,
    },
}
