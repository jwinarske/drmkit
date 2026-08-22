// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! drmkit's library-wide diagnostic surface.
//!
//! Port of `src/log.hpp` from drm-cxx @ `dc2915b`. Two knobs are exposed,
//! matching the C++ contract:
//!
//! 1. [`set_log_level`] — process-wide threshold. Records at or below the
//!    active level are emitted; the rest are dropped *before* formatting cost
//!    is paid. A call below the threshold reduces to one relaxed atomic load
//!    and a branch.
//!
//! 2. [`set_log_sink`] — process-wide redirection hook. The default sink
//!    writes to stderr (errors, warnings) or stdout (info, debug) prefixed
//!    with `[drm:LEVEL] `. Consumers can forward to `tracing`, journald,
//!    syslog, or an in-memory test buffer. [`reset_log_sink`] restores the
//!    default — the Rust spelling of the C++ `set_log_sink(nullptr)` path.
//!
//! # Deviations from the C++ original
//!
//! The C++ `set_log_level` / `set_log_sink` are documented thread-unsafe
//! ("install once during application startup, before spinning up worker
//! threads"). Here both are genuinely thread-safe: the level is an
//! [`AtomicU8`] and the sink lives behind an [`RwLock`] whose guard is
//! released before the sink runs, so a sink that logs cannot deadlock and a
//! sink that panics cannot wedge logging for the rest of the process. The
//! startup-only discipline remains good practice; it is no longer load-bearing.
//!
//! # Untrusted content in log records
//!
//! Records are delivered to the sink verbatim — no escaping, no newline
//! stripping. That matters here more than in a typical library, because some
//! of what drmkit will log is attacker-influenced: EDID monitor names and
//! serial strings come off the wire from whatever display is plugged in, and
//! a hostile or simply malformed EDID can carry newlines or ANSI escape
//! sequences. Written straight to a terminal by the built-in sink, those can
//! forge log lines or move the cursor.
//!
//! The C++ original has the same property. Rather than diverge silently, the
//! rule for the port is: **sanitize at the point the untrusted string enters a
//! record, not in the sink.** `drmkit-display` (phase 4) owns that for EDID —
//! the sink cannot tell a monitor name from a driver name once they are one
//! formatted string. Consumers who route this into a terminal and cannot
//! trust their displays should install a sink that escapes control characters.
//!
//! # Initial level
//!
//! The C++ side bakes its default in at compile time via `DRM_CXX_LOG_LEVEL`.
//! Here the initial level resolves once, on first use, from the
//! `DRMKIT_LOG_LEVEL` environment variable (`silent`/`error`/`warn`/`info`/
//! `debug`, case-insensitive, or the numeric discriminant). An unset or
//! unparseable value yields [`LogLevel::Info`], matching the C++ default. An
//! explicit [`set_log_level`] before first use always wins.
//!
//! # Example
//!
//! ```
//! use drmkit_log::{LogLevel, log_warn, set_log_level};
//!
//! set_log_level(LogLevel::Warn);
//! log_warn!("plane {} rejected modifier {:#x}", 31, 0x0100_0000_0000_0001_u64);
//! ```
//!
//! # A note on the examples
//!
//! The level and the sink are process-global, and under edition 2024 every
//! doctest in a crate is merged into one binary and run on parallel threads.
//! Examples here therefore *demonstrate* the API but never assert on what a
//! sink received: a concurrently running example would have replaced the sink
//! or moved the threshold, and the assertion would fail for reasons having
//! nothing to do with the code. The behavioral assertions live in the unit
//! tests, which serialize on a mutex the way the C++ gtest fixture does.

use std::fmt;
use std::io::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

mod macros;

#[cfg(feature = "tracing")]
mod tracing_sink;

#[cfg(feature = "tracing")]
pub use tracing_sink::install_tracing_sink;

/// Severity of a diagnostic record.
///
/// Discriminants match the C++ `drm::LogLevel` byte-for-byte, so a consumer
/// bridging both libraries can cast between them. Ordering is by verbosity:
/// a record is admitted when the active level is `>=` the record's level, so
/// [`LogLevel::Silent`] admits nothing and [`LogLevel::Debug`] admits all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum LogLevel {
    /// Emit nothing.
    Silent = 0,
    /// Unrecoverable or consumer-visible failures.
    Error = 1,
    /// Recoverable problems and fallbacks taken.
    Warn = 2,
    /// Lifecycle milestones. The default.
    Info = 3,
    /// Per-frame and per-decision detail.
    Debug = 4,
}

impl LogLevel {
    /// Every level, ascending by verbosity. [`LogLevel::Silent`] is included.
    pub const ALL: [Self; 5] = [
        Self::Silent,
        Self::Error,
        Self::Warn,
        Self::Info,
        Self::Debug,
    ];

    /// The lowercase tag the default sink prints inside `[drm:...]`.
    ///
    /// [`LogLevel::Silent`] has no tag and yields `"silent"` only for
    /// diagnostics; it is never printed, because the level gate rejects it
    /// first.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    /// Whether a record at `record` is admitted when `self` is the active
    /// level.
    ///
    /// ```
    /// use drmkit_log::LogLevel;
    /// assert!(LogLevel::Warn.admits(LogLevel::Error));
    /// assert!(!LogLevel::Warn.admits(LogLevel::Info));
    /// assert!(!LogLevel::Silent.admits(LogLevel::Error));
    /// ```
    #[must_use]
    pub const fn admits(self, record: Self) -> bool {
        // Silent must reject everything, including a hypothetical Silent
        // record; the explicit arm keeps that true independent of ordering.
        match self {
            Self::Silent => false,
            _ => (self as u8) >= (record as u8),
        }
    }

    /// Parse a level from a name (`"warn"`, case-insensitive) or from its
    /// numeric discriminant (`"2"`). Returns `None` if neither matches.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "silent" | "off" | "none" | "0" => Some(Self::Silent),
            "error" | "1" => Some(Self::Error),
            "warn" | "warning" | "2" => Some(Self::Warn),
            "info" | "3" => Some(Self::Info),
            "debug" | "trace" | "4" => Some(Self::Debug),
            _ => None,
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

/// A sink receives the record's level and its fully formatted body — no level
/// prefix, no trailing newline.
///
/// Sinks are invoked synchronously on the calling thread; a sink needing async
/// dispatch or extra buffering arranges it internally. A sink must tolerate
/// being called from any thread, and must not assume it is the only sink the
/// process will ever install.
pub type LogSink = Arc<dyn Fn(LogLevel, &str) + Send + Sync + 'static>;

/// Sentinel meaning "initial level not yet resolved from the environment".
/// Distinct from every real discriminant (0..=4).
const LEVEL_UNRESOLVED: u8 = u8::MAX;

static LEVEL: AtomicU8 = AtomicU8::new(LEVEL_UNRESOLVED);

fn sink_slot() -> &'static RwLock<Option<LogSink>> {
    static SINK: OnceLock<RwLock<Option<LogSink>>> = OnceLock::new();
    SINK.get_or_init(|| RwLock::new(None))
}

/// The active process-wide threshold.
///
/// On the first call — and only then — an unset level resolves from
/// `DRMKIT_LOG_LEVEL`, defaulting to [`LogLevel::Info`].
#[must_use]
pub fn log_level() -> LogLevel {
    match LEVEL.load(Ordering::Relaxed) {
        LEVEL_UNRESOLVED => {
            let resolved = std::env::var("DRMKIT_LOG_LEVEL")
                .ok()
                .and_then(|raw| LogLevel::parse(&raw))
                .unwrap_or(LogLevel::Info);
            // Racing callers all compute the same value from the same
            // environment, so a plain store is sufficient; whichever lands
            // last stores an identical byte. A concurrent `set_log_level`
            // may be overwritten by this resolution, which is why the
            // documented ordering is "set before first use".
            LEVEL
                .compare_exchange(
                    LEVEL_UNRESOLVED,
                    resolved as u8,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .map_or_else(from_discriminant, |_| resolved)
        }
        raw => from_discriminant(raw),
    }
}

/// Map a stored byte back to a level. Only ever called with a byte this module
/// wrote, so the fallback is unreachable in practice — but it stays total
/// rather than panicking inside the logger.
fn from_discriminant(raw: u8) -> LogLevel {
    match raw {
        0 => LogLevel::Silent,
        1 => LogLevel::Error,
        2 => LogLevel::Warn,
        4 => LogLevel::Debug,
        _ => LogLevel::Info,
    }
}

/// Set the process-wide threshold.
pub fn set_log_level(level: LogLevel) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

/// Install a sink, replacing whatever was there.
///
/// The previous sink is dropped once the last in-flight emit using it returns.
///
/// ```
/// use std::sync::{Arc, Mutex};
/// use drmkit_log::{LogLevel, log_error, set_log_level, set_log_sink};
///
/// let captured: Arc<Mutex<Vec<(LogLevel, String)>>> = Arc::new(Mutex::new(Vec::new()));
/// let sink_target = Arc::clone(&captured);
/// set_log_sink(move |level, message| {
///     sink_target.lock().unwrap().push((level, message.to_owned()));
/// });
/// set_log_level(LogLevel::Debug);
///
/// log_error!("answer is {}", 42);
/// // Not asserted on here: the sink is process-global and a concurrently
/// // running example may have replaced it. See "A note on the examples".
/// ```
pub fn set_log_sink<F>(sink: F)
where
    F: Fn(LogLevel, &str) + Send + Sync + 'static,
{
    let mut slot = sink_slot().write().unwrap_or_else(|poisoned| {
        // A sink panicked while the slot was being replaced. Logging must not
        // become the reason the process dies, so take the inner value and
        // carry on; the replacement we are about to install supersedes it.
        sink_slot().clear_poison();
        poisoned.into_inner()
    });
    *slot = Some(Arc::new(sink));
}

/// Restore the built-in stdio sink — the Rust spelling of the C++
/// `set_log_sink(nullptr)` path.
pub fn reset_log_sink() {
    let mut slot = sink_slot().write().unwrap_or_else(|poisoned| {
        sink_slot().clear_poison();
        poisoned.into_inner()
    });
    *slot = None;
}

/// The installed sink, or `None` when the built-in stdio sink is active.
///
/// The C++ `get_log_sink()` always returns a callable because its default is
/// itself a `std::function`. Here `None` distinguishes "default" from
/// "consumer-installed", which the C++ side cannot express.
#[must_use]
pub fn log_sink() -> Option<LogSink> {
    let slot = sink_slot()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    slot.clone()
}

/// The built-in sink: errors and warnings to stderr, info and debug to stdout,
/// each line prefixed `[drm:LEVEL] `.
///
/// Write failures are swallowed. A logger that panics or aborts because stderr
/// is a closed pipe would turn a diagnostic into an outage — the C++ side has
/// the same behavior by way of ignoring `fprintf`'s return.
fn default_sink(level: LogLevel, message: &str) {
    match level {
        LogLevel::Silent => {}
        LogLevel::Error | LogLevel::Warn => {
            let stream = std::io::stderr();
            let mut handle = stream.lock();
            let _ = writeln!(handle, "[drm:{}] {message}", level.tag());
        }
        LogLevel::Info | LogLevel::Debug => {
            let stream = std::io::stdout();
            let mut handle = stream.lock();
            let _ = writeln!(handle, "[drm:{}] {message}", level.tag());
        }
    }
}

/// Format and deliver a record to the active sink, bypassing the level gate.
///
/// Not part of the stable surface — the `log_*!` macros call it after they
/// have checked the level. Visible because macros expand in the caller's
/// crate.
#[doc(hidden)]
pub fn __emit(level: LogLevel, args: fmt::Arguments<'_>) {
    // Clone the Arc out and release the guard *before* running the sink: a
    // sink that logs re-enters this function, and holding a read guard across
    // that call risks deadlocking against a concurrent writer under
    // std::sync::RwLock's unspecified fairness. It also keeps a panicking
    // sink from poisoning the slot.
    let installed = log_sink();

    // `format_args!` cannot be reused, so materialize once. The C++ side hands
    // its sink a formatted std::string for the same reason.
    let message = fmt::format(args);
    installed.map_or_else(
        || default_sink(level, &message),
        |sink| sink(level, &message),
    );
}

#[cfg(test)]
mod tests;
