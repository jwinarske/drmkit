// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The devices on a seat, through libinput.

use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;

use input::event::keyboard::{KeyState, KeyboardEventTrait as _};
use input::event::pointer::{Axis, ButtonState, PointerEventTrait, PointerScrollEvent};
use input::event::switch::{Switch as LibinputSwitch, SwitchEventTrait as _, SwitchState};
use input::event::touch::{TouchEventPosition as _, TouchEventSlot as _, TouchEventTrait as _};
use input::event::{
    DeviceEvent, EventTrait, KeyboardEvent as LibinputKeyboard, PointerEvent as LibinputPointer,
    SwitchEvent as LibinputSwitchEvent, TouchEvent as LibinputTouch,
};
use input::{AsRaw as _, Device, DeviceCapability, Led, Libinput, LibinputInterface};

use crate::InputError;
use crate::event::{
    InputEvent, KeyEvent, PointerAbsolute, PointerAxis, PointerButton, PointerEvent, PointerMotion,
    ScrollSource, Switch, SwitchEvent, TouchEvent, TouchPhase,
};
use crate::leds::Leds;
use crate::libinput_log::{self, LogPriority};

/// What to open.
#[derive(Debug, Clone)]
pub struct SeatOptions {
    /// The seat's udev name. `seat0` unless the machine has more than one.
    ///
    /// A name udev has never heard of is not an error: libinput asks for the
    /// devices tagged with it, gets none, and carries on. So a typo here and
    /// a machine with no input devices look the same from the outside --
    /// [`keyboard_count`](Seat::keyboard_count) after the first dispatch is
    /// how to tell.
    pub seat: String,
    /// How much libinput should say about itself.
    ///
    /// Whatever passes this is forwarded into [`drmkit_log`] tagged
    /// `[libinput]`, where the process-wide sink and level apply on top --
    /// so a message has to pass both. The default matches libinput's own, so
    /// leaving it alone costs nothing.
    pub log_priority: LogPriority,
}

impl Default for SeatOptions {
    fn default() -> Self {
        Self {
            seat: "seat0".to_owned(),
            log_priority: LogPriority::default(),
        }
    }
}

/// Every input device on one seat.
///
/// Poll [`AsFd`], call [`dispatch`](Self::dispatch) when it is readable, and
/// act on what comes back. Nothing is delivered through a callback: an event
/// loop that has to reason about what a handler might re-enter is harder than
/// one that gets a list.
#[derive(Debug)]
pub struct Seat {
    libinput: Libinput,
    /// The keyboard-capable devices, held so their lights can be driven.
    keyboards: Vec<Device>,
    /// The last state pushed, re-applied to anything that turns up later.
    ///
    /// A keyboard plugged in mid-session, or every device coming back after a
    /// VT switch, would otherwise come up dark while xkb says Caps Lock is on.
    leds: Option<Leds>,
}

impl Seat {
    /// Open the seat, opening its devices directly.
    ///
    /// Needs read access to `/dev/input/event*`, which outside a login
    /// session means root or the `input` group. For devices that survive a VT
    /// switch, use [`Seat::open_with`] and route through a session instead.
    ///
    /// Everything typed on this machine comes through here, including into
    /// other applications: an evdev reader is a keylogger that happens to be
    /// asked politely. That is inherent to the interface -- it is why the
    /// permission is restricted -- and it is worth being deliberate about
    /// where the events go.
    ///
    /// # Errors
    ///
    /// [`InputError::Seat`] if udev has no such seat, or if there is no
    /// permission to enumerate it.
    pub fn open(options: &SeatOptions) -> Result<Self, InputError> {
        Self::open_with(options, Direct)
    }

    /// Open the seat, opening its devices through `opener`.
    ///
    /// The hook libinput calls for every privileged open and its matching
    /// close. Implement [`LibinputInterface`] over a
    /// `drmkit_session::Session` and every `/dev/input/event*` libinput
    /// touches gets the same revocable lifetime as the DRM device -- which is
    /// what makes a VT switch give the keyboard back.
    ///
    /// # Errors
    ///
    /// As [`Seat::open`].
    pub fn open_with(
        options: &SeatOptions,
        opener: impl LibinputInterface + 'static,
    ) -> Result<Self, InputError> {
        let mut libinput = Libinput::new_with_udev(opener);

        // Before assign_seat, which is what enumerates the seat: a handler
        // installed after it would miss every device-added diagnostic, which
        // is the set a consumer most wants when nothing is responding.
        //
        // SAFETY: the context is live and owned here, and `install` only
        // stores a function pointer against it.
        unsafe {
            libinput_log::install(libinput.as_raw().cast_mut().cast(), options.log_priority);
        }

        libinput
            .udev_assign_seat(&options.seat)
            .map_err(|()| InputError::Seat(options.seat.clone()))?;

        Ok(Self {
            libinput,
            keyboards: Vec::new(),
            leds: None,
        })
    }

    /// Read whatever the devices have to say, appending to `out`.
    ///
    /// Device arrivals and departures are handled here and not reported: they
    /// are this type's bookkeeping, and a consumer that wants to know which
    /// devices exist has udev for it.
    ///
    /// # Errors
    ///
    /// [`InputError::Dispatch`] if libinput cannot read its descriptor.
    pub fn dispatch(&mut self, out: &mut Vec<InputEvent>) -> Result<(), InputError> {
        self.libinput.dispatch().map_err(InputError::Dispatch)?;
        while let Some(event) = self.libinput.next() {
            match event {
                input::Event::Device(event) => self.on_device(event),
                other => out.extend(convert(other)),
            }
        }
        Ok(())
    }

    /// Close every device, keeping the seat.
    ///
    /// For a session pause. The descriptors are about to stop working, and
    /// libinput reading a revoked one produces a stream of errors rather than
    /// events.
    pub fn suspend(&mut self) {
        self.libinput.suspend();
        // Held references keep the devices alive past the suspend, and the
        // resume re-adds every one of them -- so letting go here is what stops
        // the list doubling on the second switch.
        self.keyboards.clear();
    }

    /// Reopen everything after a [`suspend`](Self::suspend).
    ///
    /// Devices arrive as ordinary additions on the next
    /// [`dispatch`](Self::dispatch), and the lights are restored with them.
    ///
    /// # Errors
    ///
    /// [`InputError::Seat`] if libinput cannot re-enumerate.
    pub fn resume(&mut self) -> Result<(), InputError> {
        self.libinput
            .resume()
            .map_err(|()| InputError::Seat("resume".to_owned()))
    }

    /// Drive the lock lights on every keyboard.
    ///
    /// xkb owns the latch and the device owns the light; this is the wire
    /// between them. Read [`Keyboard::leds`](crate::Keyboard::leds) after a
    /// key and push it here when it has changed.
    pub fn set_leds(&mut self, leds: Leds) {
        self.leds = Some(leds);
        let mask = mask(leds);
        for keyboard in &mut self.keyboards {
            keyboard.led_update(mask);
        }
    }

    /// How many keyboard-capable devices are on the seat.
    #[must_use]
    pub fn keyboard_count(&self) -> usize {
        self.keyboards.len()
    }

    fn on_device(&mut self, event: DeviceEvent) {
        match event {
            DeviceEvent::Added(added) => {
                let mut device = added.device();
                if !device.has_capability(DeviceCapability::Keyboard) {
                    return;
                }
                if let Some(leds) = self.leds {
                    // A keyboard that turns up mid-session, or every one of
                    // them after a resume, otherwise comes up dark while xkb
                    // goes on saying Caps Lock is held.
                    device.led_update(mask(leds));
                }
                self.keyboards.push(device);
            }
            DeviceEvent::Removed(removed) => {
                let device = removed.device();
                self.keyboards.retain(|held| *held != device);
            }
            _ => {}
        }
    }
}

impl AsFd for Seat {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.libinput.as_fd()
    }
}

/// Opening devices with a plain `open`.
///
/// What a process that already has permission wants, and the only thing that
/// works without a session manager.
struct Direct;

impl LibinputInterface for Direct {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<std::os::fd::OwnedFd, i32> {
        // The flags are libinput's, in the C `open(2)` encoding. CLOEXEC is
        // added because an input descriptor surviving an exec is a descriptor
        // some child can read every keystroke through.
        let flags = rustix::fs::OFlags::from_bits_retain(flags.cast_unsigned())
            | rustix::fs::OFlags::CLOEXEC;
        rustix::fs::open(path, flags, rustix::fs::Mode::empty())
            .map_err(|errno| -errno.raw_os_error())
    }

    fn close_restricted(&mut self, fd: std::os::fd::OwnedFd) {
        drop(fd);
    }
}

fn mask(leds: Leds) -> Led {
    let mut mask = Led::empty();
    mask.set(Led::CAPSLOCK, leds.caps_lock);
    mask.set(Led::NUMLOCK, leds.num_lock);
    mask.set(Led::SCROLLLOCK, leds.scroll_lock);
    mask
}

/// The device's name, for a consumer that treats a built-in trackpad
/// differently from an external mouse.
fn device_name(event: &impl EventTrait) -> Option<String> {
    Some(event.device().name().into_owned()).filter(|name| !name.is_empty())
}

fn convert(event: input::Event) -> Option<InputEvent> {
    match event {
        input::Event::Keyboard(LibinputKeyboard::Key(key)) => Some(InputEvent::Key(KeyEvent {
            time_ms: key.time(),
            key: key.key(),
            pressed: key.key_state() == KeyState::Pressed,
            repeat: false,
            sym: 0,
            utf8: String::new(),
        })),
        input::Event::Pointer(pointer) => convert_pointer(pointer).map(InputEvent::Pointer),
        input::Event::Touch(touch) => convert_touch(&touch).map(InputEvent::Touch),
        input::Event::Switch(LibinputSwitchEvent::Toggle(toggle)) => {
            let which = match toggle.switch()? {
                LibinputSwitch::Lid => Switch::Lid,
                LibinputSwitch::TabletMode => Switch::TabletMode,
                // A switch a newer libinput knows about and this does not.
                // Reporting it as one of the two here would be a lie.
                _ => return None,
            };
            Some(InputEvent::Switch(SwitchEvent {
                time_ms: toggle.time(),
                which,
                active: toggle.switch_state() == SwitchState::On,
            }))
        }
        // Tablet tools, tablet pads and gestures are not carried. Nothing in
        // the reference carries them either, and inventing an event shape for
        // hardware there is nothing here to test against would be worse than
        // the gap.
        _ => None,
    }
}

fn convert_pointer(event: LibinputPointer) -> Option<PointerEvent> {
    match event {
        LibinputPointer::Motion(motion) => Some(PointerEvent::Motion(PointerMotion {
            time_ms: motion.time(),
            dx: motion.dx(),
            dy: motion.dy(),
            device: device_name(&motion),
        })),
        LibinputPointer::MotionAbsolute(motion) => Some(PointerEvent::Absolute(PointerAbsolute {
            time_ms: motion.time(),
            x: motion.absolute_x_transformed(1),
            y: motion.absolute_y_transformed(1),
            device: device_name(&motion),
        })),
        LibinputPointer::Button(button) => Some(PointerEvent::Button(PointerButton {
            time_ms: button.time(),
            button: button.button(),
            pressed: button.button_state() == ButtonState::Pressed,
        })),
        LibinputPointer::ScrollWheel(scroll) => Some(scrolled(&scroll, ScrollSource::Wheel)),
        LibinputPointer::ScrollFinger(scroll) => Some(scrolled(&scroll, ScrollSource::Finger)),
        LibinputPointer::ScrollContinuous(scroll) => {
            Some(scrolled(&scroll, ScrollSource::Continuous))
        }
        // The pre-1.19 axis event, which libinput still emits alongside the
        // three above for exactly as long as it takes clients to move. Taking
        // both would count every scroll twice.
        _ => None,
    }
}

fn scrolled(
    event: &(impl PointerScrollEvent + PointerEventTrait),
    source: ScrollSource,
) -> PointerEvent {
    PointerEvent::Axis(PointerAxis {
        time_ms: event.time(),
        horizontal: axis(event, Axis::Horizontal),
        vertical: axis(event, Axis::Vertical),
        source,
    })
}

/// A scroll axis, or zero if this event does not carry it.
///
/// Reading one that is absent is not an error, it is a value libinput has not
/// set -- so it has to be asked about first.
fn axis(event: &impl PointerScrollEvent, axis: Axis) -> f64 {
    if event.has_axis(axis) {
        event.scroll_value(axis)
    } else {
        0.0
    }
}

fn convert_touch(event: &LibinputTouch) -> Option<TouchEvent> {
    let (phase, time_ms) = match event {
        LibinputTouch::Down(touch) => (TouchPhase::Down, touch.time()),
        LibinputTouch::Up(touch) => (TouchPhase::Up, touch.time()),
        LibinputTouch::Motion(touch) => (TouchPhase::Motion, touch.time()),
        LibinputTouch::Frame(touch) => (TouchPhase::Frame, touch.time()),
        LibinputTouch::Cancel(touch) => (TouchPhase::Cancel, touch.time()),
        _ => return None,
    };

    // Only down and motion carry a position; up, frame and cancel say which
    // point ended, not where it was.
    let (x, y) = match event {
        LibinputTouch::Down(touch) => (touch.x_transformed(1), touch.y_transformed(1)),
        LibinputTouch::Motion(touch) => (touch.x_transformed(1), touch.y_transformed(1)),
        _ => (0.0, 0.0),
    };

    // And a frame is the end of a batch rather than one point, so it has no
    // slot. The reference reports frame and cancel with a zero timestamp as
    // well, which loses the one thing a frame does carry.
    let slot = match event {
        LibinputTouch::Down(touch) => touch.slot(),
        LibinputTouch::Up(touch) => touch.slot(),
        LibinputTouch::Motion(touch) => touch.slot(),
        LibinputTouch::Cancel(touch) => touch.slot(),
        _ => None,
    };

    Some(TouchEvent {
        time_ms,
        slot: slot.map_or(-1, |slot| i32::try_from(slot).unwrap_or(-1)),
        x,
        y,
        phase,
        device: device_name(event),
    })
}
