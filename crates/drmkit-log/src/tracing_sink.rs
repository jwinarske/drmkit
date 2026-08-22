// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Bridge from the drmkit sink into [`tracing`].
//!
//! Enabled by the `tracing` feature. This is the mechanism behind plan §4.8:
//! the C++ side's per-category environment switches (`DRM_ALLOC_DEBUG`,
//! `DRM_EXT_DMABUF_DEBUG`) — thresholds that are deliberately *not* vetoed by
//! the global level — map onto `tracing`'s per-target directives natively.
//! `RUST_LOG=drmkit_planes::alloc=trace` is the drmkit spelling of
//! `DRM_ALLOC_DEBUG=1`.

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
/// ```
/// # #[cfg(feature = "tracing")]
/// drmkit_log::install_tracing_sink();
/// ```
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
