// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Routing libinput's own diagnostics into drmkit's log.
//!
//! libinput talks: device add and remove, quirk-parse failures, `client bug:
//! event processing lagging`. Left alone it writes them to its own stderr
//! handler, which escapes [`drmkit_log::set_log_sink`] and any sink a
//! consumer installed. So a handler is always installed, and everything it
//! gets is forwarded with a `[libinput]` tag.
//!
//! # Why this is hand-written
//!
//! `input-sys` blocklists `libinput_log_set_handler` -- `build.rs` line 88 --
//! and keeps the rest of the family. That is the right call for a bindgen
//! crate: the callback takes a `va_list`, which bindgen cannot express
//! portably. The fix belongs on this side, and it is small.

use std::ffi::{c_char, c_int, c_void};

/// libinput's own thresholds, from `libinput.h`.
const PRIORITY_DEBUG: c_int = 10;
const PRIORITY_INFO: c_int = 20;
const PRIORITY_ERROR: c_int = 30;

/// The most of one message that is kept.
///
/// libinput's longest are quirk-parse failures naming a file and a line, well
/// inside this. Matches the reference's buffer so a message that is truncated
/// is truncated the same way.
const BUFFER: usize = 1024;

/// How much libinput should say before this is called at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogPriority {
    /// Everything, including per-event detail.
    Debug,
    /// Device add and remove, and configuration.
    Info,
    /// Failures only. libinput's own default, and this one.
    #[default]
    Error,
}

impl LogPriority {
    const fn raw(self) -> c_int {
        match self {
            Self::Debug => PRIORITY_DEBUG,
            Self::Info => PRIORITY_INFO,
            Self::Error => PRIORITY_ERROR,
        }
    }
}

// The two calls this needs. `libinput_log_set_priority` is in `input-sys`,
// but naming it here as well keeps the pair together and this module
// independent of which version of the bindings is resolved.
//
// `struct libinput*` is passed as an opaque pointer either way, and the
// `va_list` argument is a pointer on every Linux ABI drmkit targets: on
// x86-64 and aarch64 it is an array type and decays to one, on riscv64 it is
// `void*` already, and on arm and i686 it is a single pointer-sized value.
unsafe extern "C" {
    fn libinput_log_set_handler(libinput: *mut c_void, handler: Option<Handler>);
    fn libinput_log_set_priority(libinput: *mut c_void, priority: c_int);
    fn vsnprintf(out: *mut c_char, len: usize, format: *const c_char, args: *mut c_void) -> c_int;
}

type Handler = unsafe extern "C" fn(
    libinput: *mut c_void,
    priority: c_int,
    format: *const c_char,
    args: *mut c_void,
);

/// Send this context's diagnostics to drmkit's log, at or above `priority`.
///
/// # Safety
///
/// `libinput` must be a live `struct libinput*`. The handler outlives every
/// context, so it does not have to be removed before one is dropped.
pub(crate) unsafe fn install(libinput: *mut c_void, priority: LogPriority) {
    // SAFETY: the caller guarantees the pointer. `trampoline` matches
    // libinput_log_handler's signature, and the pointer it is handed back is
    // never dereferenced.
    unsafe {
        libinput_log_set_handler(libinput, Some(trampoline));
        libinput_log_set_priority(libinput, priority.raw());
    }
}

/// What libinput calls.
///
/// A panic here would cross a C frame. Nothing in it can panic -- the
/// formatting is bounded and the forwarding takes a `&str` -- and an
/// `extern "C"` function aborts rather than unwinding if one somehow did.
unsafe extern "C" fn trampoline(
    _libinput: *mut c_void,
    priority: c_int,
    format: *const c_char,
    args: *mut c_void,
) {
    if format.is_null() {
        return;
    }
    let mut buffer = [0_u8; BUFFER];
    // SAFETY: `format` is libinput's own format string and `args` the matching
    // list, both live for this call. The buffer is what its length says, and
    // vsnprintf always terminates within it. The list is consumed once:
    // libinput's own `va_end` follows this return, and nothing else reads it.
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
    forward(priority, &buffer[..written]);
}

/// Hand one rendered message to drmkit's log.
fn forward(priority: c_int, message: &[u8]) {
    let message = String::from_utf8_lossy(message);
    let message = trim(&message);
    if message.is_empty() {
        return;
    }
    match classify(priority) {
        LogPriority::Debug => drmkit_log::log_debug!("[libinput] {message}"),
        LogPriority::Info => drmkit_log::log_info!("[libinput] {message}"),
        LogPriority::Error => drmkit_log::log_error!("[libinput] {message}"),
    }
}

/// Which band a raw libinput priority falls in.
///
/// Bands rather than exact values: libinput's are spaced ten apart precisely
/// so more can be added between them, and an unknown one should land at the
/// nearest level rather than be dropped.
fn classify(priority: c_int) -> LogPriority {
    if priority <= PRIORITY_DEBUG {
        LogPriority::Debug
    } else if priority <= PRIORITY_INFO {
        LogPriority::Info
    } else {
        LogPriority::Error
    }
}

/// Strip the line ending libinput puts on every message.
///
/// A structured sink adds its own framing, and a message that arrives with a
/// newline already on it produces a blank line in the middle of the log.
fn trim(message: &str) -> &str {
    message.trim_end_matches(['\n', '\r'])
}

#[cfg(test)]
mod tests {
    use super::{LogPriority, classify, trim};

    #[test]
    fn each_libinput_priority_lands_where_it_should() {
        assert_eq!(classify(10), LogPriority::Debug);
        assert_eq!(classify(20), LogPriority::Info);
        assert_eq!(classify(30), LogPriority::Error);
    }

    #[test]
    fn a_priority_between_the_known_ones_lands_at_the_nearest() {
        // libinput spaced them ten apart so it could add more. One that
        // arrives from a newer library should be reported, not dropped.
        assert_eq!(classify(1), LogPriority::Debug);
        assert_eq!(classify(15), LogPriority::Info);
        assert_eq!(classify(25), LogPriority::Error);
        assert_eq!(classify(999), LogPriority::Error);
    }

    #[test]
    fn the_trailing_newline_comes_off() {
        assert_eq!(trim("event5 - lagging\n"), "event5 - lagging");
        assert_eq!(trim("windows\r\n"), "windows");
        assert_eq!(trim("several\n\n\n"), "several");
        assert_eq!(trim("none"), "none");
        assert_eq!(trim("\n"), "");
    }

    #[test]
    fn a_newline_inside_the_message_stays() {
        assert_eq!(trim("two\nlines\n"), "two\nlines");
    }
}
