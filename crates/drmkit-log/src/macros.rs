// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! The `log_*!` entry points.
//!
//! Each macro checks the active level *before* touching its arguments, so a
//! record below the threshold costs one relaxed atomic load and a branch —
//! the formatting arguments are never evaluated. This mirrors the C++
//! templates, which pay formatting cost only when the level admits the record.

/// Emit at [`LogLevel::Error`](crate::LogLevel::Error).
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::__log_gated!($crate::LogLevel::Error, $($arg)*)
    };
}

/// Emit at [`LogLevel::Warn`](crate::LogLevel::Warn).
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::__log_gated!($crate::LogLevel::Warn, $($arg)*)
    };
}

/// Emit at [`LogLevel::Info`](crate::LogLevel::Info).
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::__log_gated!($crate::LogLevel::Info, $($arg)*)
    };
}

/// Emit at [`LogLevel::Debug`](crate::LogLevel::Debug).
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::__log_gated!($crate::LogLevel::Debug, $($arg)*)
    };
}

/// Emit a line belonging to an opt-in diagnostic channel — one already gated
/// by its own switch, such as the `DRM_ALLOC_DEBUG` / `DRM_EXT_DMABUF_DEBUG`
/// environment variables on the C++ side.
///
/// **Deliberately bypasses the global level**: the channel's own gate *is* its
/// threshold. This mirrors `GStreamer`, where `GST_DEBUG=cat:5` raises a named
/// category above the default threshold — asking for a category by name is not
/// vetoed by the default. Routing the other way would mean `DRM_ALLOC_DEBUG=1`
/// printed nothing at the default `Info` level, which reads as the switch
/// being broken.
///
/// What it does *not* bypass is the sink: channel output goes wherever
/// [`set_log_sink`](crate::set_log_sink) points, so it survives on targets
/// whose stderr goes nowhere.
///
/// Callers must check their own gate first — this pays full formatting cost
/// unconditionally.
///
/// ```
/// use drmkit_log::{LogLevel, log_channel};
///
/// if std::env::var_os("DRM_ALLOC_DEBUG").is_some() {
///     log_channel!(LogLevel::Debug, "layer {} -> plane {}", 0, 31);
/// }
/// // Reaches the sink even at Silent -- the channel's own gate is its
/// // threshold. Pinned by `channel_bypasses_level_but_not_sink`; not asserted
/// // here because the level and sink are process-global.
/// log_channel!(LogLevel::Debug, "always reaches the sink");
/// ```
#[macro_export]
macro_rules! log_channel {
    ($level:expr, $($arg:tt)*) => {
        $crate::__emit($level, ::core::format_args!($($arg)*))
    };
}

/// Shared body of the four level macros. Not public API.
#[doc(hidden)]
#[macro_export]
macro_rules! __log_gated {
    ($level:expr, $($arg:tt)*) => {{
        let __drmkit_level = $level;
        if $crate::log_level().admits(__drmkit_level) {
            $crate::__emit(__drmkit_level, ::core::format_args!($($arg)*));
        }
    }};
}
