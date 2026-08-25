// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The libseat backend.
//!
//! A thin adapter. Everything about *when* the seat is called lives in
//! [`Session`](crate::Session), which is tested against a fake; what is here is
//! the translation, and it is the part that cannot be exercised on a machine
//! with no seat -- which is every machine drmkit is currently built on.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::Path;
use std::rc::Rc;

use crate::SessionError;
use crate::session::{Backend, DeviceId, RawEvent};

/// How many dispatches to give the seat to activate before giving up.
///
/// A blocking dispatch does not spin, so in practice this is never reached; it
/// is here so a seat that answers without ever enabling cannot hang a process
/// at startup with nothing to show for it.
const ACTIVATION_ATTEMPTS: u32 = 64;

/// Events the seat has delivered but the session has not collected.
///
/// Shared with the listener libseat calls. Shared rather than owned because the
/// crate takes a `'static` callback at open, before there is a backend for it
/// to belong to.
type Inbox = Rc<RefCell<Vec<RawEvent>>>;

/// A session backed by libseat.
///
/// Whichever backend libseat finds: logind, seatd, or the builtin fallback for
/// a process running as root on a TTY. `LIBSEAT_BACKEND` picks one explicitly.
#[derive(Debug)]
pub struct LibseatBackend {
    seat: libseat::Seat,
    /// Open devices, by the id handed out to the session.
    ///
    /// The crate's `close_device` consumes the `Device`, so they are held here
    /// rather than handed to the caller, who only needs the descriptor.
    devices: HashMap<i32, libseat::Device>,
    next_id: i32,
    inbox: Inbox,
    /// The connection's descriptor, read once at open.
    ///
    /// The crate's accessor needs `&mut`, and a caller polling in a loop wants
    /// `&self`. It does not change for the life of the connection.
    poll_fd: Option<RawFd>,
}

impl LibseatBackend {
    /// Connect to whatever seat is available, and wait for it to activate.
    ///
    /// # Errors
    ///
    /// - [`SessionError::NoBackend`] when there is no seat. That is the
    ///   ordinary case outside a login session -- over SSH, in a container, on
    ///   a CI runner -- and a caller that only wants to look at a display can
    ///   open devices directly instead. What it gives up is surviving a VT
    ///   switch.
    /// - [`SessionError::Seat`] if the seat answers but never activates.
    pub fn open() -> Result<Self, SessionError> {
        let inbox: Inbox = Rc::new(RefCell::new(Vec::new()));
        let listener = {
            let inbox = Rc::clone(&inbox);
            move |_: &mut libseat::SeatRef, event: libseat::SeatEvent| {
                // Nothing is done here beyond recording it. libseat is calling
                // us from inside its own dispatch, and acting now would mean
                // re-entering it; the session picks these up afterwards.
                inbox.borrow_mut().push(match event {
                    libseat::SeatEvent::Enable => RawEvent::Enable,
                    libseat::SeatEvent::Disable => RawEvent::Disable,
                });
            }
        };

        let mut seat = libseat::Seat::open(listener).map_err(|errno| {
            // Which backend refused, and why, is the whole of the diagnosis
            // when a seat is expected and does not appear.
            drmkit_log::log_debug!("no seat: {errno}");
            SessionError::NoBackend
        })?;
        let poll_fd = seat.get_fd().ok().map(|fd| fd.as_raw_fd());
        let mut backend = Self {
            seat,
            devices: HashMap::new(),
            next_id: 1,
            inbox,
            poll_fd,
        };
        backend.wait_for_activation()?;
        Ok(backend)
    }

    /// The seat's name, for logging.
    pub fn name(&mut self) -> &str {
        self.seat.name()
    }

    /// Block until the seat says it is ours.
    ///
    /// A freshly opened seat is not active: libseat sends the first enable over
    /// the connection, so it only arrives once something dispatches. Skipping
    /// this leaves the first `open_device` racing the activation, which is a
    /// startup that fails on a loaded machine and works on an idle one.
    ///
    /// The enable is swallowed rather than queued. [`Session`](crate::Session)
    /// starts active, and a resume it never paused for would have it close and
    /// reopen every device for nothing.
    fn wait_for_activation(&mut self) -> Result<(), SessionError> {
        for _ in 0..ACTIVATION_ATTEMPTS {
            self.seat
                .dispatch(-1)
                .map_err(|errno| SessionError::Seat(format!("dispatch: {errno}")))?;
            if drain_through_activation(&mut self.inbox.borrow_mut()) {
                drmkit_log::log_info!("seat {} is ours", self.seat.name());
                return Ok(());
            }
        }
        Err(SessionError::Seat("the seat never activated".to_owned()))
    }
}

/// Drop everything up to and including the seat's first enable.
///
/// Returns whether one was there. What is left is what happened *after* the
/// seat became ours, which the session does have to hear about -- a seat that
/// enables and immediately disables is a switch away that has already
/// happened, and swallowing it wholesale would leave a session that thinks it
/// owns a display it does not.
fn drain_through_activation(events: &mut Vec<RawEvent>) -> bool {
    if let Some(enable) = events.iter().position(|event| *event == RawEvent::Enable) {
        events.drain(..=enable);
        true
    } else {
        // A disable before the first enable describes a seat that was never
        // ours. There is nothing for the session to stop doing.
        events.clear();
        false
    }
}

impl Drop for LibseatBackend {
    fn drop(&mut self) {
        // Closing the seat releases these anyway. Done explicitly so that stays
        // true of a backend that outlives its devices for a while, and so the
        // count the seat is holding matches the count this thinks it has.
        for (_, device) in self.devices.drain() {
            let _ = self.seat.close_device(device);
        }
    }
}

impl Backend for LibseatBackend {
    fn open_device(&mut self, path: &Path) -> Result<(DeviceId, RawFd), SessionError> {
        // The crate builds the C string with `unwrap`, so a path that is not
        // UTF-8, or has a NUL in it, panics inside the binding. Neither can
        // come from `/dev/dri`, but neither has to reach it either.
        if path.to_str().is_none_or(|path| path.contains('\0')) {
            return Err(SessionError::Seat(format!(
                "{} is not a path libseat can take",
                path.display()
            )));
        }
        let device = self
            .seat
            .open_device(&path)
            .map_err(|errno| SessionError::Seat(format!("open {}: {errno}", path.display())))?;
        let fd = device.as_fd().as_raw_fd();
        let id = self.next_id;
        self.next_id += 1;
        self.devices.insert(id, device);
        Ok((DeviceId(id), fd))
    }

    fn close_device(&mut self, id: DeviceId) -> Result<(), SessionError> {
        // One this session does not hold is not an error to pass on: it is
        // already closed, which is what was asked for.
        let Some(device) = self.devices.remove(&id.0) else {
            return Ok(());
        };
        self.seat
            .close_device(device)
            .map_err(|errno| SessionError::Seat(format!("close: {errno}")))
    }

    fn acknowledge_pause(&mut self) -> Result<(), SessionError> {
        self.seat
            .disable()
            .map_err(|errno| SessionError::Seat(format!("disable: {errno}")))
    }

    fn switch_session(&mut self, session: i32) -> Result<(), SessionError> {
        self.seat
            .switch_session(session)
            .map_err(|errno| SessionError::Seat(format!("switch to {session}: {errno}")))
    }

    fn poll_fd(&self) -> Option<RawFd> {
        self.poll_fd
    }

    fn dispatch(
        &mut self,
        timeout_ms: i32,
        events: &mut Vec<RawEvent>,
    ) -> Result<(), SessionError> {
        self.seat
            .dispatch(timeout_ms)
            .map_err(|errno| SessionError::Seat(format!("dispatch: {errno}")))?;
        // Whatever the listener recorded while that ran.
        events.append(&mut self.inbox.borrow_mut());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{RawEvent, drain_through_activation};

    #[test]
    fn activation_swallows_the_enable_that_announced_it() {
        // The session starts active. A resume it never paused for would have it
        // close and reopen every device for nothing.
        let mut events = vec![RawEvent::Enable];
        assert!(drain_through_activation(&mut events));
        assert!(events.is_empty());
    }

    #[test]
    fn a_switch_away_during_startup_still_reaches_the_session() {
        // Rare, but it is the case that matters: swallow this and the session
        // thinks it owns a display that now belongs to someone else.
        let mut events = vec![RawEvent::Enable, RawEvent::Disable];
        assert!(drain_through_activation(&mut events));
        assert_eq!(events, vec![RawEvent::Disable]);
    }

    #[test]
    fn without_an_enable_the_seat_is_not_ours_yet() {
        let mut events = vec![RawEvent::Disable];
        assert!(!drain_through_activation(&mut events));
        assert!(events.is_empty(), "and nothing is left to act on");
    }
}
