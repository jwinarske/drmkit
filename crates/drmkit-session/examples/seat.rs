// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Report whether this process can hold a seat, and what it can open on it.
//!
//! ```text
//! cargo run -p drmkit-session --example seat -- /dev/dri/card0
//! ```
//!
//! Expect it to say there is no seat unless it is run from a login session on
//! the machine's own console. Over SSH logind reports the session busy, seatd
//! is not running, and libseat's builtin backend wants root and a TTY.
//! `LIBSEAT_BACKEND` forces one.

use drmkit_session::{Session, SessionError};

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();

    let mut backend = match drmkit_session::LibseatBackend::open() {
        Ok(backend) => backend,
        Err(SessionError::NoBackend) => {
            println!("no seat here: not a login session, no seatd, no root TTY");
            return;
        }
        Err(error) => {
            eprintln!("seat: {error}");
            std::process::exit(1);
        }
    };

    println!("seat: {}", backend.name());
    let mut session = Session::new(backend);

    for path in &paths {
        match session.take_device(path) {
            Ok(device) => println!("  {path}: fd {}", device.fd),
            Err(error) => println!("  {path}: {error}"),
        }
    }

    if paths.is_empty() {
        println!("pass device paths to open some");
    }
}
