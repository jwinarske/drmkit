// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The libinput side.
//!
//! No devices here, and none needed for the part that is easy to get wrong:
//! libinput will happily be asked to open a path that does not exist, and
//! complains through exactly the handler this crate installs.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use input::{AsRaw as _, Libinput, LibinputInterface};

use crate::libinput_log::{self, LogPriority};

/// The log sink and level are process-wide, so only one test drives them.
fn log_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An opener that refuses everything, which is all libinput needs to have
/// something to complain about.
struct Refuse;

impl LibinputInterface for Refuse {
    fn open_restricted(&mut self, _path: &Path, _flags: i32) -> Result<std::os::fd::OwnedFd, i32> {
        Err(-libc_enoent())
    }

    fn close_restricted(&mut self, _fd: std::os::fd::OwnedFd) {}
}

const fn libc_enoent() -> i32 {
    2
}

#[test]
fn libinput_diagnostics_arrive_through_drmkit_log() {
    // The whole shim end to end: libinput's own vararg formatting, the
    // va_list crossing into vsnprintf, the priority mapping, and the routing
    // into a sink a consumer installed.
    let _guard = log_guard();

    let captured: Arc<Mutex<Vec<(drmkit_log::LogLevel, String)>>> = Arc::default();
    let sink = Arc::clone(&captured);
    drmkit_log::set_log_sink(move |level, message| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((level, message.to_owned()));
    });
    let level = drmkit_log::log_level();
    drmkit_log::set_log_level(drmkit_log::LogLevel::Debug);

    let mut libinput = Libinput::new_from_path(Refuse);
    // SAFETY: the context is live for the rest of this test.
    unsafe {
        libinput_log::install(libinput.as_raw().cast_mut().cast(), LogPriority::Debug);
    }
    assert!(
        libinput
            .path_add_device("/dev/input/there-is-no-such-device")
            .is_none(),
        "the opener refused it"
    );

    drmkit_log::set_log_level(level);
    drmkit_log::reset_log_sink();

    let captured = captured
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        captured
            .iter()
            .any(|(_, message)| message.contains("[libinput]")),
        "libinput said nothing through the sink: {captured:?}"
    );
    assert!(
        captured
            .iter()
            .any(|(_, message)| message.contains("there-is-no-such-device")),
        "and it was the message about the device: {captured:?}"
    );
    assert!(
        captured.iter().all(|(_, message)| !message.ends_with('\n')),
        "with libinput's newline taken off: {captured:?}"
    );
}

#[test]
fn an_overlong_message_is_truncated_rather_than_overrunning() {
    // libinput puts the path it was given into the message, so a long enough
    // path pushes it past the buffer. Nothing here should be reading past the
    // end of it, and the run is under ASan in CI.
    let _guard = log_guard();

    let captured: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&captured);
    drmkit_log::set_log_sink(move |_, message| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message.to_owned());
    });
    let level = drmkit_log::log_level();
    drmkit_log::set_log_level(drmkit_log::LogLevel::Debug);

    let mut libinput = Libinput::new_from_path(Refuse);
    // SAFETY: the context is live for the rest of this test.
    unsafe {
        libinput_log::install(libinput.as_raw().cast_mut().cast(), LogPriority::Debug);
    }
    let path = format!("/dev/input/{}", "e".repeat(4096));
    libinput.path_add_device(&path);

    drmkit_log::set_log_level(level);
    drmkit_log::reset_log_sink();

    let captured = captured
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let longest = captured.iter().map(String::len).max().unwrap_or(0);
    assert!(longest > 0, "libinput said nothing");
    assert!(
        longest < 1100,
        "a 4 KiB path came through whole: {longest} bytes"
    );
}

#[test]
fn a_seat_that_does_not_exist_opens_with_nothing_on_it() {
    // Worth pinning because it is the opposite of what the name suggests.
    // libinput asks udev for the devices tagged with this seat and gets none,
    // which is not an error -- so a typo in the seat name is indistinguishable
    // from a machine with no input devices, and the only signal either way is
    // that nothing ever arrives.
    let seat = crate::seat::Seat::open(&crate::seat::SeatOptions {
        seat: "there-is-no-such-seat".to_owned(),
        ..crate::seat::SeatOptions::default()
    })
    .expect("opens anyway");
    assert_eq!(seat.keyboard_count(), 0);
}
