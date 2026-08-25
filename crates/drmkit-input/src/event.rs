// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! What comes off an input device.
//!
//! Deliberately plain data. Nothing here borrows from libinput, so an event
//! can be queued, compared, or written into a test without a device in sight.

/// A key going down or coming up.
///
/// `key` is the evdev code -- `KEY_A`, `KEY_LEFTSHIFT` -- with no xkb offset
/// applied. [`Keyboard::process_key`](crate::Keyboard::process_key) fills in
/// [`sym`](Self::sym) and [`utf8`](Self::utf8); until it has, both are empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyEvent {
    /// libinput's timestamp, in milliseconds.
    pub time_ms: u32,
    /// The evdev keycode, without xkb's `+ 8`.
    pub key: u32,
    /// Down, rather than up.
    pub pressed: bool,
    /// Synthesized by the repeater rather than produced by a device.
    pub repeat: bool,
    /// The xkb keysym this resolved to.
    pub sym: u32,
    /// What the key types, if anything.
    ///
    /// The reference carries a `char[8]`, so it truncates anything longer
    /// than seven bytes. Nothing a single keysym produces reaches that, but
    /// there is no reason to inherit the cliff.
    pub utf8: String,
}

/// A pointer moving.
#[derive(Debug, Clone, PartialEq)]
pub struct PointerMotion {
    /// libinput's timestamp, in milliseconds.
    pub time_ms: u32,
    /// Horizontal movement, in libinput's accelerated units.
    pub dx: f64,
    /// Vertical movement, likewise.
    pub dy: f64,
    /// The device this came from, if libinput named it.
    ///
    /// Owned rather than borrowed. The reference hands out the `const char*`
    /// libinput owns and tells the caller it is valid for the duration of the
    /// callback -- which is true, and is a rule a consumer only has to forget
    /// once. It is there so a consumer can treat a built-in trackpad
    /// differently from an external mouse, and that decision is made per
    /// event, not per byte.
    pub device: Option<String>,
}

/// A pointer button going down or coming up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerButton {
    /// libinput's timestamp, in milliseconds.
    pub time_ms: u32,
    /// The evdev button code: `BTN_LEFT`, `BTN_RIGHT`, and so on.
    pub button: u32,
    /// Down, rather than up.
    pub pressed: bool,
}

/// Where a scroll came from.
///
/// It changes what the scroll means. A wheel click is a discrete step and
/// should move a fixed amount; a finger on a touchpad is continuous and wants
/// kinetic scrolling, and its end is a real event -- the finger lifting --
/// rather than the absence of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollSource {
    /// A mouse wheel, in discrete clicks.
    Wheel,
    /// One or more fingers on a touchpad.
    Finger,
    /// A continuous source, such as a button held and the device moved.
    Continuous,
}

/// A scroll.
#[derive(Debug, Clone, PartialEq)]
pub struct PointerAxis {
    /// libinput's timestamp, in milliseconds.
    pub time_ms: u32,
    /// Horizontal scroll.
    pub horizontal: f64,
    /// Vertical scroll.
    pub vertical: f64,
    /// What did the scrolling.
    pub source: ScrollSource,
}

/// A pointer reporting where it is rather than how far it moved.
///
/// Graphics tablets, and the absolute pointing device a virtual machine
/// gives its guest -- which is how a lot of this gets run before it reaches
/// hardware.
#[derive(Debug, Clone, PartialEq)]
pub struct PointerAbsolute {
    /// libinput's timestamp, in milliseconds.
    pub time_ms: u32,
    /// Position across the device's range, in `[0, 1)`.
    ///
    /// Normalized here because the raw value is in millimetres, and scaling
    /// that to a render extent needs the digitizer's physical size -- which
    /// the consumer would have to go and ask for.
    pub x: f64,
    /// Position down the device's range, likewise.
    pub y: f64,
    /// The device this came from, if libinput named it.
    pub device: Option<String>,
}

/// Anything a pointer does.
#[derive(Debug, Clone, PartialEq)]
pub enum PointerEvent {
    /// The pointer moved.
    Motion(PointerMotion),
    /// The pointer reported where it is.
    Absolute(PointerAbsolute),
    /// A button changed.
    Button(PointerButton),
    /// A scroll happened.
    Axis(PointerAxis),
}

/// What a touch point is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchPhase {
    /// A finger landed.
    Down,
    /// A finger lifted.
    Up,
    /// A finger moved.
    Motion,
    /// The end of one batch of simultaneous touch changes.
    Frame,
    /// The sequence was abandoned -- a palm, or a device going away.
    Cancel,
}

/// A touch point.
#[derive(Debug, Clone, PartialEq)]
pub struct TouchEvent {
    /// libinput's timestamp, in milliseconds.
    pub time_ms: u32,
    /// The multi-touch slot, which identifies one finger across a sequence.
    pub slot: i32,
    /// Position in device coordinates.
    pub x: f64,
    /// Position in device coordinates.
    pub y: f64,
    /// What the point is doing.
    pub phase: TouchPhase,
    /// The device this came from, if libinput named it.
    pub device: Option<String>,
}

/// Which physical switch changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// The lid.
    Lid,
    /// The convertible's tablet-mode hinge.
    TabletMode,
}

/// A switch changing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchEvent {
    /// libinput's timestamp, in milliseconds.
    pub time_ms: u32,
    /// Which switch.
    pub which: Switch,
    /// Closed, or in tablet mode.
    pub active: bool,
}

/// Anything an input device produces.
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    /// A key.
    Key(KeyEvent),
    /// A pointer.
    Pointer(PointerEvent),
    /// A touch point.
    Touch(TouchEvent),
    /// A switch.
    Switch(SwitchEvent),
}
