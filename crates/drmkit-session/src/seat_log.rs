// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Routing libseat's own diagnostics into drmkit's log.
//!
//! libseat says why it could not open a seat -- no logind session, seatd's
//! socket missing, a `LIBSEAT_BACKEND` that names nothing -- and that is the
//! whole of the diagnosis when a seat is expected and does not appear. Left
//! alone, it writes to its own stderr handler, at a level that defaults to
//! silent. So a handler is installed before every open, at error level, and
//! everything it gets is forwarded with a `[libseat]` tag. Port of upstream's
//! `seat_log_trampoline`.
//!
//! # Why this is hand-written
//!
//! `libseat-sys` binds `libseat_set_log_level` but not
//! `libseat_set_log_handler`, whose callback takes a `va_list`. The binding's
//! own `custom_logger` gets round that with a C shim, and sends what arrives to
//! the `log` crate, which `drmkit-log` does not read. The `va_list` is a
//! pointer on every ABI drmkit targets, so it can go straight to `vsnprintf`
//! here, as `drmkit-input` already does for libinput.

use std::ffi::{c_char, c_int, c_void};

/// libseat's levels, from `libseat.h`.
const LEVEL_ERROR: c_int = 1;
const LEVEL_INFO: c_int = 2;
const LEVEL_DEBUG: c_int = 3;

/// The most of one message that is kept. Matches the reference's buffer, so a
/// message that is truncated is truncated the same way.
const BUFFER: usize = 1024;

// The `va_list` argument is a pointer on every Linux ABI drmkit targets: on
// x86-64 and aarch64 it is an array type and decays to one, on riscv64 it is
// `void*` already, and on arm and i686 it is a single pointer-sized value.
// libseat's level is an enum, passed as an `int`.
unsafe extern "C" {
    fn libseat_set_log_handler(handler: Option<Handler>);
    fn libseat_set_log_level(level: c_int);
    fn vsnprintf(out: *mut c_char, len: usize, format: *const c_char, args: *mut c_void) -> c_int;
}

type Handler = unsafe extern "C" fn(level: c_int, format: *const c_char, args: *mut c_void);

/// Send libseat's errors to drmkit's log.
///
/// Process-wide, as libseat's handler is. Installed before each open, as the
/// reference does, so a failure to open is reported; doing it again is
/// harmless.
pub(crate) fn install() {
    // SAFETY: `trampoline` matches `libseat_log_func`, and both calls only
    // store what they are given.
    unsafe {
        libseat_set_log_handler(Some(trampoline));
        libseat_set_log_level(LEVEL_ERROR);
    }
}

/// What libseat calls.
///
/// A panic here would cross a C frame. Nothing in it can panic -- the
/// formatting is bounded and the forwarding takes a `&str` -- and an
/// `extern "C"` function aborts rather than unwinding if one somehow did.
unsafe extern "C" fn trampoline(level: c_int, format: *const c_char, args: *mut c_void) {
    if format.is_null() {
        return;
    }
    let mut buffer = [0_u8; BUFFER];
    // SAFETY: `format` is libseat's own format string and `args` the matching
    // list, both live for this call. The buffer is what its length says, and
    // vsnprintf always terminates within it. The list is consumed once:
    // libseat's own `va_end` follows this return, and nothing else reads it.
    let written = unsafe {
        vsnprintf(
            buffer.as_mut_ptr().cast::<c_char>(),
            buffer.len(),
            format,
            args,
        )
    };
    let Ok(written) = usize::try_from(written) else {
        // Negative means an output error, which for a fixed buffer means a
        // format string vsnprintf could not handle. Nothing to report.
        return;
    };
    // vsnprintf returns the length it *would* have written.
    let written = written.min(buffer.len() - 1);
    forward(level, &buffer[..written]);
}

/// Hand one rendered message to drmkit's log.
fn forward(level: c_int, message: &[u8]) {
    let message = String::from_utf8_lossy(message);
    let message = message.trim_end_matches(['\n', '\r']);
    if message.is_empty() {
        return;
    }
    match classify(level) {
        Level::Error => drmkit_log::log_error!("[libseat] {message}"),
        Level::Info => drmkit_log::log_info!("[libseat] {message}"),
        Level::Debug => drmkit_log::log_debug!("[libseat] {message}"),
        Level::Other => drmkit_log::log_warn!("[libseat] {message}"),
    }
}

/// Where a libseat level is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Error,
    Info,
    Debug,
    /// `LIBSEAT_LOG_LEVEL_SILENT`, which nothing should be logged at, or a
    /// level a newer libseat adds. Reported rather than dropped, but not
    /// ranked as an error -- the reference's choice.
    Other,
}

fn classify(level: c_int) -> Level {
    match level {
        LEVEL_ERROR => Level::Error,
        LEVEL_INFO => Level::Info,
        LEVEL_DEBUG => Level::Debug,
        _ => Level::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::{Level, classify};

    #[test]
    fn each_libseat_level_lands_where_the_reference_puts_it() {
        assert_eq!(classify(1), Level::Error);
        assert_eq!(classify(2), Level::Info);
        assert_eq!(classify(3), Level::Debug);
    }

    #[test]
    fn silent_and_unknown_levels_are_warnings() {
        assert_eq!(classify(0), Level::Other);
        assert_eq!(classify(4), Level::Other);
        assert_eq!(classify(-1), Level::Other);
    }
}
