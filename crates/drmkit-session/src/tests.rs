// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The session sequencing, against a seat that does what it is told.
//!
//! None of this can be tested against a real seat on the machines this is
//! built on: over SSH logind reports the session busy, seatd is not running,
//! and the builtin backend wants root and a TTY. That is exactly why the
//! sequencing lives above a trait.

use std::path::{Path, PathBuf};

use crate::session::{Backend, DeviceId, RawEvent};
use crate::{Session, SessionError, SessionEvent};

/// What the fake seat was asked to do, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Open(PathBuf),
    Close(DeviceId),
    Acknowledge,
    Switch(i32),
}

/// A seat that opens whatever it is asked for and records everything.
#[derive(Debug, Default)]
struct FakeSeat {
    calls: Vec<Call>,
    /// Handed out in order, so a reopen is visibly a different descriptor.
    next_id: i32,
    next_fd: i32,
    /// Queued to be delivered by the next dispatch.
    deliver: Vec<RawEvent>,
    /// Refuse the next open, to test a resume that cannot complete.
    fail_next_open: bool,
}

impl FakeSeat {
    fn new() -> Self {
        Self {
            next_id: 1,
            next_fd: 100,
            ..Self::default()
        }
    }

    fn queue(&mut self, event: RawEvent) {
        self.deliver.push(event);
    }

    fn opens(&self) -> Vec<&PathBuf> {
        self.calls
            .iter()
            .filter_map(|call| match call {
                Call::Open(path) => Some(path),
                _ => None,
            })
            .collect()
    }
}

impl Backend for FakeSeat {
    fn open_device(&mut self, path: &Path) -> Result<(DeviceId, i32), SessionError> {
        self.calls.push(Call::Open(path.to_path_buf()));
        if self.fail_next_open {
            self.fail_next_open = false;
            return Err(SessionError::Seat("refused".to_owned()));
        }
        let id = DeviceId(self.next_id);
        let fd = self.next_fd;
        self.next_id += 1;
        self.next_fd += 1;
        Ok((id, fd))
    }

    fn close_device(&mut self, id: DeviceId) -> Result<(), SessionError> {
        self.calls.push(Call::Close(id));
        Ok(())
    }

    fn acknowledge_pause(&mut self) -> Result<(), SessionError> {
        self.calls.push(Call::Acknowledge);
        Ok(())
    }

    fn switch_session(&mut self, session: i32) -> Result<(), SessionError> {
        self.calls.push(Call::Switch(session));
        Ok(())
    }

    fn poll_fd(&self) -> Option<i32> {
        Some(7)
    }

    fn dispatch(&mut self, _timeout: i32, events: &mut Vec<RawEvent>) -> Result<(), SessionError> {
        events.append(&mut self.deliver);
        Ok(())
    }
}

/// A session holding one card.
fn with_card() -> Session<FakeSeat> {
    let mut session = Session::new(FakeSeat::new());
    session.take_device("/dev/dri/card0").expect("take");
    session
}

#[test]
fn taking_a_device_records_it() {
    let mut session = Session::new(FakeSeat::new());
    let device = session.take_device("/dev/dri/card0").expect("take");

    assert_eq!(device.path, Path::new("/dev/dri/card0"));
    assert_eq!(device.fd, 100);
    assert_eq!(session.devices().len(), 1);
    assert!(session.is_active());
}

#[test]
fn taking_the_same_path_twice_holds_one_device() {
    // The seat would happily give a second descriptor, and then a resume has
    // two to reopen and the caller two to keep straight.
    let mut session = Session::new(FakeSeat::new());
    let first = session.take_device("/dev/dri/card0").expect("take");
    let again = session.take_device("/dev/dri/card0").expect("take again");

    assert_eq!(first, again);
    assert_eq!(session.devices().len(), 1);
}

#[test]
fn a_pause_is_acknowledged_and_reported() {
    let mut session = with_card();
    session.backend_mut().queue(RawEvent::Disable);
    session.dispatch(0).expect("dispatch");

    assert!(!session.is_active(), "the display is not ours any more");
    assert_eq!(session.next_event(), Some(SessionEvent::Paused));
    assert!(
        session.backend().calls.contains(&Call::Acknowledge),
        "an unacknowledged pause stalls the switch"
    );
}

#[test]
fn a_device_cannot_be_taken_while_paused() {
    // The seat would refuse anyway; failing here says why.
    let mut session = with_card();
    session.backend_mut().queue(RawEvent::Disable);
    session.dispatch(0).expect("dispatch");

    assert_eq!(
        session.take_device("/dev/dri/card1").unwrap_err(),
        SessionError::Inactive
    );
}

#[test]
fn a_resume_replaces_every_descriptor() {
    // Every per-descriptor kernel object is gone, so reusing the old one would
    // work from objects the kernel has forgotten.
    let mut session = with_card();
    let before = session.devices()[0].fd;

    session.backend_mut().queue(RawEvent::Disable);
    session.backend_mut().queue(RawEvent::Enable);
    session.dispatch(0).expect("dispatch");

    assert_eq!(session.next_event(), Some(SessionEvent::Paused));
    let Some(SessionEvent::Resumed { devices }) = session.next_event() else {
        panic!("expected a resume");
    };
    assert_eq!(devices.len(), 1);
    assert_ne!(devices[0].fd, before, "a fresh descriptor, not the old one");
    assert_eq!(devices[0].path, Path::new("/dev/dri/card0"));
    assert!(session.is_active());
}

#[test]
fn a_resume_closes_before_it_reopens() {
    // The seat tracks one open device per handle. Opening the same path again
    // without closing leaks its handle for as long as the session runs.
    let mut session = with_card();
    session.backend_mut().queue(RawEvent::Disable);
    session.backend_mut().queue(RawEvent::Enable);
    session.dispatch(0).expect("dispatch");

    let calls = &session.backend().calls;
    let close = calls
        .iter()
        .position(|call| matches!(call, Call::Close(_)))
        .expect("the old handle is closed");
    let reopen = calls
        .iter()
        .rposition(|call| matches!(call, Call::Open(_)))
        .expect("reopened");
    assert!(close < reopen, "closed before reopened: {calls:?}");
}

#[test]
fn every_held_device_comes_back() {
    let mut session = Session::new(FakeSeat::new());
    for path in ["/dev/dri/card0", "/dev/dri/card1", "/dev/input/event0"] {
        session.take_device(path).expect("take");
    }

    session.backend_mut().queue(RawEvent::Disable);
    session.backend_mut().queue(RawEvent::Enable);
    session.dispatch(0).expect("dispatch");
    session.next_event();

    let Some(SessionEvent::Resumed { devices }) = session.next_event() else {
        panic!("expected a resume");
    };
    assert_eq!(devices.len(), 3);
    let paths: Vec<&Path> = devices.iter().map(|d| d.path.as_path()).collect();
    assert_eq!(
        paths,
        vec![
            Path::new("/dev/dri/card0"),
            Path::new("/dev/dri/card1"),
            Path::new("/dev/input/event0"),
        ],
        "and in the order they were taken"
    );
}

#[test]
fn a_released_device_does_not_come_back() {
    let mut session = Session::new(FakeSeat::new());
    session.take_device("/dev/dri/card0").expect("take");
    session.take_device("/dev/dri/card1").expect("take");
    session.release_device("/dev/dri/card0").expect("release");

    session.backend_mut().queue(RawEvent::Disable);
    session.backend_mut().queue(RawEvent::Enable);
    session.dispatch(0).expect("dispatch");
    session.next_event();

    let Some(SessionEvent::Resumed { devices }) = session.next_event() else {
        panic!("expected a resume");
    };
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].path, Path::new("/dev/dri/card1"));
}

#[test]
fn releasing_something_never_taken_is_an_error() {
    let mut session = with_card();
    assert_eq!(
        session.release_device("/dev/dri/card9").unwrap_err(),
        SessionError::UnknownDevice
    );
    assert_eq!(session.devices().len(), 1, "and holds what it had");
}

#[test]
fn a_repeated_pause_is_reported_once() {
    // A seat that says it twice is not a reason to tell the caller twice; they
    // have already stopped.
    let mut session = with_card();
    session.backend_mut().queue(RawEvent::Disable);
    session.backend_mut().queue(RawEvent::Disable);
    session.dispatch(0).expect("dispatch");

    assert_eq!(session.next_event(), Some(SessionEvent::Paused));
    assert_eq!(session.next_event(), None, "not twice");
}

#[test]
fn a_resume_without_a_pause_changes_nothing() {
    // Seats send an enable at startup on some backends. The session is already
    // active, so there is nothing to rebuild and nothing to tell the caller.
    let mut session = with_card();
    let before = session.devices().to_vec();

    session.backend_mut().queue(RawEvent::Enable);
    session.dispatch(0).expect("dispatch");

    assert_eq!(session.next_event(), None);
    assert_eq!(session.devices(), before, "descriptors are untouched");
}

#[test]
fn a_resume_that_cannot_reopen_reports_it() {
    // The display is not coming back on its own, and a caller that never
    // learns is a caller that sits there.
    let mut session = with_card();
    session.backend_mut().fail_next_open = true;
    session.backend_mut().queue(RawEvent::Disable);
    session.backend_mut().queue(RawEvent::Enable);

    let result = session.dispatch(0);
    assert!(matches!(result, Err(SessionError::Seat(_))), "{result:?}");
    // The pause still reached the caller: they must stop either way.
    assert_eq!(session.next_event(), Some(SessionEvent::Paused));
}

#[test]
fn events_arrive_in_the_order_they_happened() {
    let mut session = with_card();
    for _ in 0..3 {
        session.backend_mut().queue(RawEvent::Disable);
        session.backend_mut().queue(RawEvent::Enable);
    }
    session.dispatch(0).expect("dispatch");

    let mut seen = Vec::new();
    while let Some(event) = session.next_event() {
        seen.push(matches!(event, SessionEvent::Paused));
    }
    assert_eq!(
        seen,
        vec![true, false, true, false, true, false],
        "three switches away and back"
    );
}

#[test]
fn switching_asks_the_seat_and_waits() {
    // Asynchronous: the seat pauses when it honours the request. Nothing
    // changes at the moment of asking.
    let mut session = with_card();
    session.switch_session(3).expect("switch");

    assert!(session.backend().calls.contains(&Call::Switch(3)));
    assert!(
        session.is_active(),
        "still ours until the seat says otherwise"
    );
    assert_eq!(session.next_event(), None);
}

#[test]
fn the_poll_descriptor_comes_from_the_seat() {
    let session = with_card();
    assert_eq!(session.poll_fd(), Some(7));
}

#[test]
fn dispatching_nothing_is_quiet() {
    let mut session = with_card();
    session.dispatch(0).expect("dispatch");
    assert_eq!(session.next_event(), None);
    assert!(session.is_active());
    assert_eq!(
        session.backend().opens().len(),
        1,
        "an idle dispatch reopens nothing"
    );
}
