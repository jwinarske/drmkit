// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Error type for `drmkit-dumb`.

use rustix::io::Errno;

/// Errors from dumb-buffer allocation and framebuffer registration.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DumbError {
    /// Zero-sized, zero-format, or out-of-range dimensions.
    /// `errc::invalid_argument`.
    #[error("invalid dumb buffer configuration: {reason}")]
    InvalidConfig {
        /// What was wrong.
        reason: &'static str,
    },

    /// The `FourCC` is not one [`Buffer::create_planar`](crate::Buffer::create_planar)
    /// recognizes. `errc::not_supported`.
    ///
    /// Callers can fall back to [`Buffer::create`](crate::Buffer::create) for
    /// single-plane formats.
    #[error("format {fourcc:#010x} is not a supported semi-planar layout")]
    UnsupportedFormat {
        /// The requested `FourCC`.
        fourcc: u32,
    },

    /// A size computation would not fit the kernel's 32-bit field.
    /// `errc::value_too_large`.
    #[error("dumb buffer geometry overflows a 32-bit kernel field: {what}")]
    TooLarge {
        /// Which computation overflowed.
        what: &'static str,
    },

    /// An ioctl or mmap failed.
    #[error("dumb buffer operation failed: {0}")]
    Io(#[from] Errno),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, DumbError>;
