// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Error type for `drmkit-modeset`.

use rustix::io::Errno;

/// Errors from mode selection and page-flip dispatch.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModesetError {
    /// No usable mode. `errc::no_such_device`.
    #[error("no usable mode")]
    NoModes,

    /// Nothing to wait on: the dispatcher holds no DRM descriptor and no
    /// foreign sources. `errc::bad_file_descriptor`.
    ///
    /// Surfaced up front rather than blocking forever on an empty epoll.
    #[error("dispatcher has nothing to wait on")]
    NothingToWaitOn,

    /// The wait budget elapsed with no events. `errc::timed_out`.
    ///
    /// **Never** produced by an interrupted wait — see
    /// [`PageFlip::dispatch`](crate::PageFlip::dispatch) and invariant 3.
    #[error("dispatch timed out")]
    TimedOut,

    /// A descriptor was rejected: negative, or already registered.
    /// `errc::invalid_argument`.
    #[error("invalid or duplicate source descriptor")]
    InvalidSource,

    /// An underlying syscall failed.
    #[error("modeset operation failed: {0}")]
    Io(#[from] Errno),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, ModesetError>;
