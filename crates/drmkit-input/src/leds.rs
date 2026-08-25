// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The lock lights.

/// The lock state, as the keyboard's lights would show it.
///
/// Two sides use this and neither owns it: xkb tracks the latch, and the
/// device carries the light. Keeping them together is the caller's job --
/// read [`Keyboard::leds`](crate::Keyboard::leds) after a key, and when it
/// differs from last time push it to [`Seat::set_leds`](crate::Seat::set_leds).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Leds {
    /// Caps Lock.
    pub caps_lock: bool,
    /// Num Lock.
    pub num_lock: bool,
    /// Scroll Lock.
    ///
    /// Dead on most layouts: xkb's default compat has no mod-mapping for
    /// `<SCLK>`, so nothing sets it and
    /// [`Keyboard::set_leds`](crate::Keyboard::set_leds) cannot make it true.
    pub scroll_lock: bool,
}
