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
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers with `..Default::default()`, which a new field does not break"
)]
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
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers, with no default to fill a new field from"
)]
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
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers, with no default to fill a new field from"
)]
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
#[non_exhaustive]
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
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers, with no default to fill a new field from"
)]
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
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers, with no default to fill a new field from"
)]
pub struct PointerAbsolute {
    /// libinput's timestamp, in milliseconds.
    pub time_ms: u32,
    /// Position across the device's range, in `[0, 1)`.
    ///
    /// Normalized here because the raw value is in millimeters, and scaling
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers, with no default to fill a new field from"
)]
pub struct TouchEvent {
    /// libinput's timestamp, in milliseconds.
    pub time_ms: u32,
    /// The multi-touch slot, which identifies one finger across a sequence.
    ///
    /// `-1` on a [`TouchPhase::Frame`], which closes a batch rather than
    /// naming a finger.
    pub slot: i32,
    /// Position across the device's range, normalized to `[0, 1)`; zero for
    /// up, frame and cancel.
    pub x: f64,
    /// Position across the device's range, normalized to `[0, 1)`; zero for
    /// up, frame and cancel.
    pub y: f64,
    /// What the point is doing.
    pub phase: TouchPhase,
    /// The device this came from, if libinput named it.
    pub device: Option<String>,
}

/// Which physical switch changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Switch {
    /// The lid.
    Lid,
    /// The convertible's tablet-mode hinge.
    TabletMode,
}

/// A switch changing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers, with no default to fill a new field from"
)]
pub struct SwitchEvent {
    /// libinput's timestamp, in milliseconds.
    pub time_ms: u32,
    /// Which switch.
    pub which: Switch,
    /// Closed, or in tablet mode.
    pub active: bool,
}

/// Which gesture a touchpad recognized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GestureKind {
    /// Fingers moving together in one direction. Three or more: two fingers
    /// moving together are a scroll, and arrive as a
    /// [`PointerAxis`] from [`ScrollSource::Finger`].
    Swipe,
    /// Fingers moving apart, together, or around each other.
    Pinch,
    /// Fingers resting without moving.
    ///
    /// What ends a kinetic scroll: the fingers come back down and the
    /// content should stop where it is.
    Hold,
}

/// Where a gesture is in its sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GesturePhase {
    /// The gesture was recognized.
    Begin,
    /// The fingers moved. A [`GestureKind::Hold`] has none of these.
    Update,
    /// The fingers lifted, and what the gesture did stands.
    End,
    /// The gesture was abandoned -- more fingers landed and it became a
    /// different one, or the device went away -- and what it did should be
    /// undone.
    Cancel,
}

/// A step of a touchpad gesture.
///
/// One sequence at a time per device: a `Begin`, any number of `Update`s,
/// then an `End` or a `Cancel`, with [`kind`](Self::kind) and
/// [`fingers`](Self::fingers) the same throughout.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers, with no default to fill a new field from"
)]
pub struct GestureEvent {
    /// libinput's timestamp, in milliseconds.
    pub time_ms: u32,
    /// Which gesture.
    pub kind: GestureKind,
    /// Where in its sequence.
    pub phase: GesturePhase,
    /// How many fingers are making it.
    pub fingers: u32,
    /// Horizontal movement of the fingers' center since the last event, in
    /// the accelerated units [`PointerMotion`] uses. Zero unless this is the
    /// `Update` of a swipe or a pinch.
    pub dx: f64,
    /// Vertical movement, likewise.
    pub dy: f64,
    /// How far apart the fingers are against where they began: `1.0` at the
    /// start, `2.0` at twice the distance, `0.5` at half.
    ///
    /// Absolute, not a step. `1.0` for anything that is not a pinch; a
    /// pinch's `End` and `Cancel` repeat its last `Update`'s.
    pub scale: f64,
    /// How far the fingers turned around their center since the last event,
    /// in degrees, clockwise positive. Zero unless this is a pinch's
    /// `Update`.
    ///
    /// A step, where [`scale`](Self::scale) is a total: that is how libinput
    /// reports the two, and adding the steps up is the consumer's to do.
    pub rotation: f64,
    /// The device this came from, if libinput named it.
    pub device: Option<String>,
}

/// What a device can produce.
///
/// One device is often several of these: a keyboard with a built-in
/// trackpoint is a keyboard and a pointer, and a touchpad is a pointer that
/// also has gestures -- which is how to tell it from a mouse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(
    clippy::exhaustive_structs,
    clippy::struct_excessive_bools,
    reason = "built by callers with `..Default::default()`, which a new field does not break; \
              and each is an independent capability, not a state"
)]
pub struct DeviceCapabilities {
    /// Keys.
    pub keyboard: bool,
    /// Motion, buttons and scrolling.
    pub pointer: bool,
    /// Touch points on a screen.
    pub touch: bool,
    /// Touchpad gestures.
    pub gesture: bool,
    /// A lid or tablet-mode switch.
    pub switch: bool,
    /// A tablet's pen or other tool. Its events are not carried.
    pub tablet_tool: bool,
    /// A tablet's own buttons, rings and strips. Not carried either.
    pub tablet_pad: bool,
}

/// Whether a device arrived or left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeviceChange {
    /// The device is on the seat and its events will follow.
    Added,
    /// The device is gone, and nothing more will come from it.
    Removed,
}

/// A device arriving on the seat or leaving it.
///
/// Every device present when the seat opens is reported as added by the
/// first dispatch, so a consumer that tracks these from the start always
/// knows what is there. A suspend removes every device and a resume adds
/// them again; the removals are reported by the first dispatch after the
/// resume, ahead of the additions.
///
/// libinput has already tidied up by the time a removal arrives: keys and
/// buttons the device held are released and its touch points canceled, each
/// as an ordinary event ahead of this one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::exhaustive_structs,
    reason = "built by callers, with no default to fill a new field from"
)]
pub struct DeviceEvent {
    /// Arrived, or left.
    pub change: DeviceChange,
    /// The kernel's name for the device node: `event5`.
    ///
    /// What identifies one device between its arrival and its departure.
    /// [`name`](Self::name) does not: two of the same mouse share one. The
    /// kernel hands the node name to the next device once this one is gone.
    pub sysname: String,
    /// The device's description of itself, if it gave one. What the
    /// `device` field of the other events carries.
    pub name: Option<String>,
    /// The USB or Bluetooth vendor id; zero where the bus has none.
    pub vendor: u32,
    /// The product id, likewise.
    pub product: u32,
    /// What the device can produce.
    pub capabilities: DeviceCapabilities,
}

/// Anything an input device produces.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum InputEvent {
    /// A key.
    Key(KeyEvent),
    /// A pointer.
    Pointer(PointerEvent),
    /// A touch point.
    Touch(TouchEvent),
    /// A switch.
    Switch(SwitchEvent),
    /// A touchpad gesture.
    Gesture(GestureEvent),
    /// A device arriving or leaving.
    Device(DeviceEvent),
}
