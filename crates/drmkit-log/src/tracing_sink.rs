// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Bridge from the drmkit sink into [`tracing`].
//!
//! Enabled by the `tracing` feature. Everything arrives under the single
//! `drmkit` target, so `RUST_LOG=drmkit=debug` admits it. The per-category
//! environment switches (`DRM_ALLOC_DEBUG`, `DRM_EXT_DMABUF_DEBUG`) -- the
//! C++ side's thresholds, deliberately *not* vetoed by the global level --
//! still gate their channels: `tracing` filters what they emit, but does not
//! replace them.

use crate::{LogLevel, set_log_level, set_log_sink};

/// Route every admitted record into `tracing` under the `drmkit` target.
///
/// Also raises the drmkit threshold to [`LogLevel::Debug`], because once
/// `tracing` owns filtering, a second threshold in front of it would silently
/// veto directives the consumer asked for — the exact failure the C++ channel
/// carve-out exists to avoid. Call [`set_log_level`] afterwards if a coarse
/// pre-filter is genuinely wanted.
///
/// Installing is idempotent in effect, though each call replaces the sink.
///
/// ```no_run
/// drmkit_log::install_tracing_sink();
/// ```
///
/// Marked `no_run`: this replaces the process-global sink and raises the
/// global threshold, so actually running it would perturb every other example
/// in the merged doctest binary. It is still compiled and type-checked.
pub fn install_tracing_sink() {
    set_log_sink(|level, message| match level {
        LogLevel::Error => tracing::error!(target: "drmkit", "{message}"),
        LogLevel::Warn => tracing::warn!(target: "drmkit", "{message}"),
        LogLevel::Info => tracing::info!(target: "drmkit", "{message}"),
        LogLevel::Debug => tracing::debug!(target: "drmkit", "{message}"),
        // Unreachable: the level gate rejects Silent before emit, and
        // `log_channel!` callers pass a real severity. Dropping is the only
        // sane reading of "emit at severity silent".
        LogLevel::Silent => {}
    });
    set_log_level(LogLevel::Debug);
}
