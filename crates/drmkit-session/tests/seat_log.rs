// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! libseat's own diagnostics arrive through drmkit's log.
//!
//! The whole path end to end: libseat formatting its message, the `va_list`
//! crossing into `vsnprintf`, the level mapping, and the routing into a sink
//! a consumer installed. A `LIBSEAT_BACKEND` that names no backend makes
//! libseat refuse the open with an error naming it -- on any machine, seat or
//! not.
//!
//! Its own binary, without the test harness: setting the variable is only
//! sound while nothing else is running, and the harness runs cases on threads.

use std::sync::{Arc, Mutex, PoisonError};

use drmkit_log::LogLevel;
use drmkit_session::{LibseatBackend, SessionError};

const BACKEND: &str = "drmkit-no-such-backend";

fn main() {
    // SAFETY: this binary is single-threaded -- `harness = false` -- and
    // nothing has started a thread yet.
    unsafe { std::env::set_var("LIBSEAT_BACKEND", BACKEND) };

    let captured: Arc<Mutex<Vec<(LogLevel, String)>>> = Arc::default();
    let sink = Arc::clone(&captured);
    drmkit_log::set_log_sink(move |level, message| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((level, message.to_owned()));
    });

    let opened = LibseatBackend::open();
    drmkit_log::reset_log_sink();

    assert!(
        matches!(opened, Err(SessionError::NoBackend)),
        "a backend that does not exist opens no seat: {opened:?}"
    );
    let captured = captured.lock().unwrap_or_else(PoisonError::into_inner);
    let reported = captured
        .iter()
        .find(|(_, message)| message.starts_with("[libseat]") && message.contains(BACKEND));
    let Some((level, message)) = reported else {
        panic!("libseat's refusal did not reach the sink: {captured:?}");
    };
    assert_eq!(*level, LogLevel::Error, "{message}");
    assert!(
        !message.ends_with('\n'),
        "the line ending comes off: {message:?}"
    );
    println!("seat_log: {message}");
}
