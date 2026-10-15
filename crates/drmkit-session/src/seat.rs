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
use std::time::{Duration, Instant};

use crate::SessionError;
use crate::session::{Backend, DeviceId, RawEvent};

/// How long [`LibseatBackend::open`] waits for the seat to activate.
///
/// Port of upstream's `Seat::k_default_enable_timeout` (`60c6f10`).
pub const DEFAULT_ENABLE_TIMEOUT: Duration = Duration::from_secs(3);

/// The longest single dispatch while the wait is bounded, so the deadline is
/// checked often enough to be kept.
const DISPATCH_SLICE_MS: u128 = 100;

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
    ///   The same when the backend connects but does not activate the seat
    ///   within [`DEFAULT_ENABLE_TIMEOUT`] -- seatd over SSH, with no VT to
    ///   take -- so the caller falls back the same way.
    ///   And the same when a dispatch fails before the seat activates, as the
    ///   reference falls back on it.
    pub fn open() -> Result<Self, SessionError> {
        Self::open_within(Some(DEFAULT_ENABLE_TIMEOUT))
    }

    /// As [`open`](Self::open), waiting up to `timeout` for the seat to
    /// activate; `None` waits indefinitely.
    ///
    /// Without a bound, a backend that connects but never activates the seat
    /// blocked forever, and SIGINT could not break the wait, so the process
    /// kept DRM master (drm-cxx `60c6f10`). On timeout the seat is closed.
    ///
    /// # Errors
    ///
    /// As [`open`](Self::open).
    pub fn open_within(timeout: Option<Duration>) -> Result<Self, SessionError> {
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

        // Before the open, so libseat's reasons for refusing one are heard.
        crate::seat_log::install();
        let mut seat = libseat::Seat::open(listener).map_err(|errno| {
            // Which backend refused, and why, is the whole of the diagnosis
            // when a seat is expected and does not appear.
            drmkit_log::log_error!("seat: opening a seat failed: {errno}");
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
        // Dropped on a timeout, which closes the seat.
        backend.wait_for_activation(timeout)?;
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
    fn wait_for_activation(&mut self, timeout: Option<Duration>) -> Result<(), SessionError> {
        let Self { seat, inbox, .. } = self;
        wait_until_active(
            |timeout_ms| {
                seat.dispatch(timeout_ms)
                    .map(|_| ())
                    .map_err(|errno| errno.to_string())
            },
            inbox,
            timeout,
        )?;
        drmkit_log::log_info!("seat {} is ours", self.seat.name());
        Ok(())
    }
}

/// Dispatch until the seat's first enable arrives in `inbox`, or `timeout`
/// passes; `None` waits indefinitely.
///
/// `dispatch` is the seat's, taking a timeout in milliseconds with `-1` to
/// block. While bounded, no single dispatch outlasts [`DISPATCH_SLICE_MS`] or
/// the time left, so the deadline is kept. A timeout is
/// [`SessionError::NoBackend`], the "no seat" a caller already falls back on,
/// and so is a dispatch that fails before the seat is ours, logged as an
/// error, as the reference treats both.
fn wait_until_active(
    mut dispatch: impl FnMut(i32) -> Result<(), String>,
    inbox: &Inbox,
    timeout: Option<Duration>,
) -> Result<(), SessionError> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    loop {
        let timeout_ms = match deadline {
            None => -1,
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    drmkit_log::log_warn!(
                        "seat not enabled within {} ms; open devices directly instead",
                        timeout.unwrap_or_default().as_millis()
                    );
                    return Err(SessionError::NoBackend);
                }
                // At most DISPATCH_SLICE_MS, so it fits.
                i32::try_from(left.as_millis().min(DISPATCH_SLICE_MS)).unwrap_or(0)
            }
        };
        if let Err(error) = dispatch(timeout_ms) {
            drmkit_log::log_error!("seat: dispatch before the seat was enabled failed: {error}");
            return Err(SessionError::NoBackend);
        }
        if drain_through_activation(&mut inbox.borrow_mut()) {
            return Ok(());
        }
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
    use super::*;

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

    /// A seat that answers but never enables -- seatd over SSH, with no VT to
    /// take -- gives up within the bound, as "no seat", and never blocks
    /// longer than the time left. Port of upstream's
    /// `OpenTimesOutWhenSeatNeverEnabled` (`60c6f10`).
    #[test]
    fn a_seat_that_never_enables_times_out_as_no_seat() {
        let inbox: Inbox = Rc::new(RefCell::new(Vec::new()));
        let mut waits = Vec::new();
        let started = Instant::now();
        let result = wait_until_active(
            |timeout_ms| {
                waits.push(timeout_ms);
                std::thread::sleep(Duration::from_millis(
                    u64::try_from(timeout_ms.max(0)).unwrap_or(0),
                ));
                Ok(())
            },
            &inbox,
            Some(Duration::from_millis(50)),
        );
        assert_eq!(result, Err(SessionError::NoBackend));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!waits.is_empty());
        assert!(
            waits.iter().all(|ms| (0..=50).contains(ms)),
            "no dispatch may block past the deadline: {waits:?}"
        );
    }

    /// The first enable ends the wait, and what came after it stays for the
    /// session.
    #[test]
    fn the_first_enable_ends_the_wait() {
        let inbox: Inbox = Rc::new(RefCell::new(Vec::new()));
        let mut calls = 0;
        let result = wait_until_active(
            |_| {
                calls += 1;
                if calls == 3 {
                    inbox
                        .borrow_mut()
                        .extend([RawEvent::Enable, RawEvent::Disable]);
                }
                Ok(())
            },
            &inbox,
            Some(DEFAULT_ENABLE_TIMEOUT),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(calls, 3);
        assert_eq!(*inbox.borrow(), [RawEvent::Disable]);
    }

    /// Unbounded, each dispatch blocks until the seat says something.
    #[test]
    fn an_unbounded_wait_blocks_in_dispatch() {
        let inbox: Inbox = Rc::new(RefCell::new(Vec::new()));
        let mut waits = Vec::new();
        let result = wait_until_active(
            |timeout_ms| {
                waits.push(timeout_ms);
                inbox.borrow_mut().push(RawEvent::Enable);
                Ok(())
            },
            &inbox,
            None,
        );
        assert_eq!(result, Ok(()));
        assert_eq!(waits, [-1]);
    }

    /// A dispatch that fails before the seat is ours ends the wait at once as
    /// "no seat", the answer a caller falls back on, as the reference gives
    /// it -- and the failure is logged as an error, so the fallback is not
    /// silent.
    #[test]
    fn a_failed_dispatch_is_reported() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&captured);
        drmkit_log::set_log_sink(move |level, message| {
            sink.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((level, message.to_owned()));
        });
        let inbox: Inbox = Rc::new(RefCell::new(Vec::new()));
        let started = Instant::now();
        let result = wait_until_active(
            |_| Err("EIO".to_owned()),
            &inbox,
            Some(DEFAULT_ENABLE_TIMEOUT),
        );
        drmkit_log::reset_log_sink();

        assert_eq!(result, Err(SessionError::NoBackend));
        assert!(started.elapsed() < DEFAULT_ENABLE_TIMEOUT, "not a timeout");
        let captured = captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            captured.iter().any(|(level, message)| {
                *level == drmkit_log::LogLevel::Error && message.contains("EIO")
            }),
            "{captured:?}"
        );
    }
}
